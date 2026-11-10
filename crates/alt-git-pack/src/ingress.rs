//! Bounded network pack decoding. No refs or filesystem catalog are mutated.
use crate::{PackError, PlainRepresentation, RepresentationLimits, delta};
use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    sync::Arc,
};

#[derive(Clone, Copy, Debug)]
pub struct IngressLimits {
    pub wire_bytes: u64,
    pub objects: usize,
    pub object_bytes: u64,
    /// Total inflated entry bytes, including delta programs.
    pub inflated_bytes: u64,
    /// Total canonical bytes, including externally fetched thin-pack bases.
    pub resolved_bytes: u64,
    pub delta_depth: usize,
    pub steps: usize,
}
pub struct BaseReader<'a> {
    pub kind: ObjectKind,
    pub bytes: u64,
    pub reader: Box<dyn Read + 'a>,
}
/// The adapter must restrict this lookup to authorized, pinned repository bytes.
/// A caller-supplied object ID is not permission to look outside that scope.
pub trait BaseSource {
    fn open(&mut self, oid: ObjectId) -> Result<Option<BaseReader<'_>>, PackError>;
}
pub struct NoBases;
impl BaseSource for NoBases {
    fn open(&mut self, _: ObjectId) -> Result<Option<BaseReader<'_>>, PackError> {
        Ok(None)
    }
}
#[derive(Clone, Debug)]
pub struct IngressObject {
    pub oid: ObjectId,
    pub kind: ObjectKind,
    pub data: Arc<Vec<u8>>,
}
pub struct IngressPack {
    pub objects: Vec<IngressObject>,
    pub wire_bytes: u64,
    /// Optional derived plain encodings, separately bounded from decoded data.
    pub representations: Vec<PlainRepresentation>,
}
struct Raw {
    offset: usize,
    kind: Kind,
    payload: Vec<u8>,
    encoded: Option<Vec<u8>>,
}
enum Kind {
    Plain(ObjectKind),
    Ofs(usize),
    Ref(ObjectId),
}
#[derive(Clone)]
struct Resolved {
    object: IngressObject,
    depth: usize,
}
fn fail(reason: &'static str) -> PackError {
    PackError::Format(reason)
}
fn spend(budget: &mut u64, bytes: u64) -> Result<(), PackError> {
    *budget = budget
        .checked_sub(bytes)
        .ok_or(PackError::Limit("pack byte budget"))?;
    Ok(())
}
fn step(budget: &mut usize) -> Result<(), PackError> {
    *budget = budget
        .checked_sub(1)
        .ok_or(PackError::Limit("pack work budget"))?;
    Ok(())
}
fn byte(bytes: &[u8], pos: &mut usize) -> Result<u8, PackError> {
    let b = *bytes
        .get(*pos)
        .ok_or_else(|| fail("truncated pack header"))?;
    *pos += 1;
    Ok(b)
}
fn header(bytes: &[u8], pos: &mut usize, algo: HashAlgo) -> Result<(Kind, u64), PackError> {
    let offset = *pos;
    let mut b = byte(bytes, pos)?;
    let typ = (b >> 4) & 7;
    let mut size = u64::from(b & 15);
    let mut shift = 4u32;
    while b & 128 != 0 {
        b = byte(bytes, pos)?;
        let part = u64::from(b & 127);
        if shift >= 64 || part > (u64::MAX >> shift) {
            return Err(fail("pack size overflow"));
        }
        size |= part << shift;
        shift += 7;
    }
    let kind = match typ {
        1 => Kind::Plain(ObjectKind::Commit),
        2 => Kind::Plain(ObjectKind::Tree),
        3 => Kind::Plain(ObjectKind::Blob),
        4 => Kind::Plain(ObjectKind::Tag),
        6 => {
            let mut b = byte(bytes, pos)?;
            let mut distance = u64::from(b & 127);
            while b & 128 != 0 {
                b = byte(bytes, pos)?;
                distance = distance
                    .checked_add(1)
                    .and_then(|v| v.checked_mul(128))
                    .and_then(|v| v.checked_add(u64::from(b & 127)))
                    .ok_or_else(|| fail("pack delta offset overflow"))?;
            }
            let distance =
                usize::try_from(distance).map_err(|_| fail("pack delta offset range"))?;
            let base = offset
                .checked_sub(distance)
                .filter(|p| distance > 0 && *p >= 12)
                .ok_or_else(|| fail("pack delta base offset"))?;
            Kind::Ofs(base)
        }
        7 => {
            let raw = bytes
                .get(
                    *pos..pos
                        .checked_add(algo.raw_len())
                        .ok_or_else(|| fail("pack offset overflow"))?,
                )
                .ok_or_else(|| fail("truncated pack ref delta"))?;
            *pos += algo.raw_len();
            Kind::Ref(ObjectId::from_bytes(algo, raw).map_err(|_| fail("pack ref delta id"))?)
        }
        _ => return Err(fail("invalid pack entry type")),
    };
    Ok((kind, size))
}
fn inflate(bytes: &[u8], size: u64) -> Result<(Vec<u8>, usize), PackError> {
    let size = usize::try_from(size).map_err(|_| fail("pack expansion exceeds header"))?;
    // the output grows with what the stream really holds, not with what the header claims
    let (output, consumed) =
        alt_inflate::inflate(alt_inflate::Format::Zlib, bytes, size).map_err(|error| {
            fail(match error {
                alt_inflate::Error::TooLong => "pack expansion exceeds header",
                alt_inflate::Error::Truncated => "truncated pack zlib stream",
                _ => "invalid pack zlib stream",
            })
        })?;
    if output.len() != size {
        return Err(fail("pack inflated size mismatch"));
    }
    Ok((output, consumed))
}
fn checksum(bytes: &[u8], algo: HashAlgo) -> Vec<u8> {
    match algo {
        HashAlgo::Sha1 => alt_hash::sha1(bytes).to_vec(),
        HashAlgo::Sha256 => alt_hash::sha256(bytes).to_vec(),
    }
}
/// Decode a complete pack within explicit resource bounds. The caller owns
/// transport deadlines and staging quotas. Returned objects are canonical but
/// still require graph validation and publication authorization.
pub fn ingest_pack(
    source: &mut dyn Read,
    algo: HashAlgo,
    limits: IngressLimits,
    bases: &mut impl BaseSource,
) -> Result<IngressPack, PackError> {
    ingest_pack_with_representations(source, algo, limits, bases, RepresentationLimits::default())
}

/// Decode with the same integrity/resource rules as `ingest_pack`, optionally
/// retaining bounded plain zlib streams for later verified encoding reuse.
pub fn ingest_pack_with_representations(
    source: &mut dyn Read,
    algo: HashAlgo,
    limits: IngressLimits,
    bases: &mut impl BaseSource,
    retention: RepresentationLimits,
) -> Result<IngressPack, PackError> {
    if limits.wire_bytes > usize::MAX as u64
        || limits.object_bytes > usize::MAX as u64
        || limits.objects == 0
        || limits.steps == 0
    {
        return Err(PackError::Limit("invalid pack limits"));
    }
    let mut bytes = Vec::new();
    source
        .take(limits.wire_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limits.wire_bytes {
        return Err(PackError::Limit("pack wire limit"));
    }
    if bytes.len() < 12 + algo.raw_len() || &bytes[..4] != b"PACK" {
        return Err(fail("invalid pack header"));
    }
    let version = u32::from_be_bytes(bytes[4..8].try_into().unwrap());
    if !matches!(version, 2 | 3) {
        return Err(fail("unsupported pack version"));
    }
    let count = u32::from_be_bytes(bytes[8..12].try_into().unwrap()) as usize;
    if count > limits.objects {
        return Err(PackError::Limit("pack object count limit"));
    }
    let end = bytes.len() - algo.raw_len();
    if checksum(&bytes[..end], algo) != bytes[end..] {
        return Err(fail("pack trailer mismatch"));
    }
    let mut steps = limits.steps;
    let mut inflated = limits.inflated_bytes;
    let mut canonical = limits.resolved_bytes;
    let mut entries = Vec::new();
    let mut retained_bytes = 0usize;
    let mut retained_objects = 0usize;
    let mut pos = 12;
    for _ in 0..count {
        step(&mut steps)?;
        let offset = pos;
        let (kind, size) = header(&bytes[..end], &mut pos, algo)?;
        if size > limits.object_bytes {
            return Err(PackError::Limit("pack entry size limit"));
        }
        spend(&mut inflated, size)?;
        let (payload, used) = inflate(&bytes[pos..end], size)?;
        let encoded = if matches!(kind, Kind::Plain(_))
            && size <= retention.object_bytes as u64
            && used <= retention.object_bytes
            && retained_objects < retention.objects
            && used <= retention.bytes.saturating_sub(retained_bytes)
        {
            retained_bytes += used;
            retained_objects += 1;
            Some(bytes[pos..pos + used].to_vec())
        } else {
            None
        };
        pos = pos
            .checked_add(used)
            .ok_or_else(|| fail("pack offset overflow"))?;
        entries.push(Raw {
            offset,
            kind,
            payload,
            encoded,
        });
    }
    if pos != end {
        return Err(fail("pack count or trailing bytes"));
    }
    let by_offset: BTreeMap<_, _> = entries
        .iter()
        .enumerate()
        .map(|(i, e)| (e.offset, i))
        .collect();
    let mut resolved: Vec<Option<Resolved>> = vec![None; count];
    let mut by_oid: BTreeMap<ObjectId, Resolved> = BTreeMap::new();
    for (i, entry) in entries.iter_mut().enumerate() {
        step(&mut steps)?;
        if let Kind::Plain(kind) = entry.kind {
            spend(&mut canonical, entry.payload.len() as u64)?;
            let data = Arc::new(std::mem::take(&mut entry.payload));
            let oid = ObjectId::hash_object(algo, kind, &data);
            let value = Resolved {
                object: IngressObject { oid, kind, data },
                depth: 0,
            };
            insert(&mut by_oid, value.clone())?;
            resolved[i] = Some(value);
        }
    }
    let mut attempted = BTreeSet::new();
    let mut external = 0usize;
    loop {
        let mut progress = false;
        let mut unresolved = false;
        for (i, entry) in entries.iter().enumerate() {
            step(&mut steps)?;
            if resolved[i].is_some() {
                continue;
            }
            unresolved = true;
            let base = match entry.kind {
                Kind::Plain(_) => unreachable!(),
                Kind::Ofs(offset) => resolved[*by_offset
                    .get(&offset)
                    .ok_or_else(|| fail("pack delta offset is not an entry"))?]
                .clone(),
                Kind::Ref(oid) => {
                    if !by_oid.contains_key(&oid) && attempted.insert(oid) {
                        step(&mut steps)?;
                        if let Some(mut base) = bases.open(oid)? {
                            external = external
                                .checked_add(1)
                                .ok_or_else(|| fail("pack external base count"))?;
                            if count.saturating_add(external) > limits.objects
                                || base.bytes > limits.object_bytes
                            {
                                return Err(PackError::Limit("pack external base limit"));
                            }
                            spend(&mut canonical, base.bytes)?;
                            let mut data = Vec::new();
                            base.reader
                                .by_ref()
                                .take(base.bytes.saturating_add(1))
                                .read_to_end(&mut data)?;
                            if data.len() as u64 != base.bytes
                                || ObjectId::hash_object(algo, base.kind, &data) != oid
                            {
                                return Err(fail("pack external base identity"));
                            }
                            insert(
                                &mut by_oid,
                                Resolved {
                                    object: IngressObject {
                                        oid,
                                        kind: base.kind,
                                        data: Arc::new(data),
                                    },
                                    depth: 0,
                                },
                            )?;
                        }
                    }
                    by_oid.get(&oid).cloned()
                }
            };
            let Some(base) = base else {
                continue;
            };
            let depth = base
                .depth
                .checked_add(1)
                .ok_or_else(|| fail("pack delta depth"))?;
            if depth > limits.delta_depth {
                return Err(PackError::Limit("pack delta depth limit"));
            }
            let data = delta::apply_bounded(
                &base.object.data,
                &entry.payload,
                limits.object_bytes.min(canonical),
            )?;
            spend(&mut canonical, data.len() as u64)?;
            let oid = ObjectId::hash_object(algo, base.object.kind, &data);
            let value = Resolved {
                object: IngressObject {
                    oid,
                    kind: base.object.kind,
                    data: Arc::new(data),
                },
                depth,
            };
            insert(&mut by_oid, value.clone())?;
            resolved[i] = Some(value);
            progress = true;
        }
        if !unresolved {
            break;
        }
        if !progress {
            return Err(fail("pack delta base missing or cycle"));
        }
    }
    let objects: Vec<_> = resolved
        .into_iter()
        .map(|v| v.expect("all entries resolved").object)
        .collect();
    let representations = entries
        .into_iter()
        .zip(&objects)
        .filter_map(|(entry, object)| {
            entry.encoded.map(|encoded| {
                PlainRepresentation::from_ingress(
                    object.oid,
                    object.kind,
                    object.data.len(),
                    encoded,
                )
            })
        })
        .collect();
    Ok(IngressPack {
        objects,
        wire_bytes: bytes.len() as u64,
        representations,
    })
}
fn insert(map: &mut BTreeMap<ObjectId, Resolved>, value: Resolved) -> Result<(), PackError> {
    if let Some(old) = map.get(&value.object.oid) {
        if old.object.kind != value.object.kind || old.object.data != value.object.data {
            return Err(fail("pack object id collision"));
        }
    } else {
        map.insert(value.object.oid, value);
    }
    Ok(())
}
