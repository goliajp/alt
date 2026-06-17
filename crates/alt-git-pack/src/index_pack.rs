//! `git index-pack` equivalent: given a `.pack` file on disk, produce the
//! sibling `.idx` so [`IndexedPack`](crate::IndexedPack) can open it.
//!
//! ## Why this module
//!
//! Packs received from a smart-http fetch arrive as a *stream*: the wire
//! sends bytes, we save them to disk, and now we have a pack with no idx.
//! The idx is what makes random object lookup possible (and what every
//! downstream — alt-import, alt-export, lineage delta — depends on).
//! `git index-pack` builds it; we need our own.
//!
//! ## Scope
//!
//! Non-thin packs only: every delta's base must be present in this same
//! pack. That matches our fetch policy (we don't claim `thin-pack`
//! capability, so the server sends self-contained packs). Thin-pack
//! ingestion belongs to a later step when push-side `--fix-thin` is
//! needed.
//!
//! ## Algorithm
//!
//! 1. Map the pack, read the header (`PACK` + version + count).
//! 2. Stream the entries sequentially. For each:
//!    - Parse the entry header (kind + inflated size + delta-base info).
//!    - Drive a `ZlibDecoder` to read the compressed payload; record the
//!      consumed compressed-byte count so we know where the next entry
//!      starts and so we can crc32 the raw bytes for the idx.
//!    - Record (offset, kind/base, compressed-length, payload).
//! 3. Resolve every entry to a canonical `(ObjectKind, Vec<u8>)`:
//!    - Plain entries resolve directly.
//!    - OFS_DELTA: base is at a known earlier offset → resolve recursively
//!      (memoised, so a delta chain of N entries costs N inflations).
//!    - REF_DELTA: scan the resolved table for the base oid (we don't know
//!      a delta's base oid until its base is resolved, so we iterate until
//!      stable). For self-contained packs this terminates in ≤ depth
//!      iterations.
//! 4. Hash each entry's canonical payload to its oid; emit the v2 idx
//!    (fanout, sorted oids, per-entry crc32s, offsets, pack trailer, idx
//!    checksum).
//!
//! ## Memory
//!
//! Resolved object payloads are cached in a `Vec` keyed by entry index for
//! the duration of indexing. For a 100 MB packed-out repo this peaks at a
//! few hundred MB of decompressed memory; tighter cost control (LRU on
//! base cache) is a follow-up.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use flate2::Crc;
use flate2::read::ZlibDecoder;
use memmap2::Mmap;

use crate::PackError;
use crate::delta;

/// Result of indexing a `.pack` file.
#[derive(Debug)]
pub struct IndexedPackOnDisk {
    /// Path of the pack we read (possibly renamed to its content-derived
    /// `pack-<trailer-hex>.pack` form when [`index_pack`] was called with
    /// `rename = true`).
    pub pack_path: PathBuf,
    /// Path of the newly-written `.idx`.
    pub idx_path: PathBuf,
    /// Pack trailer (file-name stem when renamed).
    pub trailer: Vec<u8>,
    /// Object count.
    pub objects: u32,
}

/// Build a `.idx` for an existing `.pack` file. The idx is written next
/// to the pack (`<pack>.idx`). When `rename` is true, both files are then
/// renamed to git's conventional `pack-<trailer-hex>.{pack,idx}` form.
///
/// `algo` must match the algorithm used for object hashing in this pack
/// (sha-1 unless the repo is explicitly sha-256).
pub fn index_pack(
    pack_path: &Path,
    algo: HashAlgo,
    rename: bool,
) -> Result<IndexedPackOnDisk, PackError> {
    let file = File::open(pack_path)?;
    // Safety: the file is not modified for the lifetime of this map (we
    // hold the only handle), and we never expose `&[u8]` past this fn.
    let map = unsafe { Mmap::map(&file)? };
    let trailer_len = algo.raw_len();
    if map.len() < 12 + trailer_len || &map[..4] != b"PACK" {
        return Err(PackError::Format("not a pack file (bad magic)"));
    }
    let version = read_be_u32(&map, 4);
    if !matches!(version, 2 | 3) {
        return Err(PackError::Format("unsupported pack version"));
    }
    let count = read_be_u32(&map, 8);

    let trailer = map[map.len() - trailer_len..].to_vec();
    // verify the pack trailer matches a fresh hash of the body — protects
    // against truncated / corrupt downloads
    verify_trailer(&map, algo, &trailer)?;

    // -- pass 1: walk entries sequentially, recording raw form --
    let mut entries: Vec<RawEntry> = Vec::with_capacity(count as usize);
    let mut pos: u64 = 12;
    let body_end: u64 = (map.len() - trailer_len) as u64;
    for _ in 0..count {
        if pos >= body_end {
            return Err(PackError::Format("pack header count exceeds body"));
        }
        let entry = read_entry(&map, pos, algo, body_end)?;
        pos = entry.next_offset;
        entries.push(entry);
    }
    if pos != body_end {
        return Err(PackError::Format("pack body has trailing bytes"));
    }

    // -- pass 2: resolve each entry's canonical (kind, data) --
    let resolved = resolve_all(&entries)?;

    // -- pass 3: compute oids, build idx --
    let mut oid_entries: Vec<(ObjectId, u64, u32)> = Vec::with_capacity(entries.len());
    for (i, entry) in entries.iter().enumerate() {
        let r = &resolved[i];
        let oid = ObjectId::hash_object(algo, r.kind, &r.data);
        oid_entries.push((oid, entry.offset, entry.crc32));
    }

    let idx_bytes = build_idx(algo, &mut oid_entries, &trailer);
    let idx_path = pack_path.with_extension("idx");
    let tmp_idx = pack_path.with_extension("idx.tmp");
    {
        let mut f = File::create(&tmp_idx)?;
        f.write_all(&idx_bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp_idx, &idx_path)?;

    let (final_pack, final_idx) = if rename {
        let dir = pack_path.parent().unwrap_or_else(|| Path::new("."));
        let hex: String = trailer.iter().map(|b| format!("{b:02x}")).collect();
        let renamed_pack = dir.join(format!("pack-{hex}.pack"));
        let renamed_idx = dir.join(format!("pack-{hex}.idx"));
        if pack_path != renamed_pack {
            std::fs::rename(pack_path, &renamed_pack)?;
        }
        if idx_path != renamed_idx {
            std::fs::rename(&idx_path, &renamed_idx)?;
        }
        (renamed_pack, renamed_idx)
    } else {
        (pack_path.to_path_buf(), idx_path)
    };

    Ok(IndexedPackOnDisk {
        pack_path: final_pack,
        idx_path: final_idx,
        trailer,
        objects: count,
    })
}

/// A pack entry as found on disk, with enough info to resolve it later.
struct RawEntry {
    offset: u64,
    /// Byte after the compressed payload — where the next entry begins.
    next_offset: u64,
    kind: RawEntryKind,
    /// Inflated payload (canonical for `Plain`, delta script for delta
    /// entries).
    payload: Vec<u8>,
    /// CRC32 of the raw on-disk bytes (header + compressed payload).
    crc32: u32,
}

enum RawEntryKind {
    Plain(ObjectKind),
    Ofs { base_at: u64 },
    Ref { base: ObjectId },
}

fn read_entry(
    map: &[u8],
    offset: u64,
    algo: HashAlgo,
    body_end: u64,
) -> Result<RawEntry, PackError> {
    let mut pos = offset as usize;
    let header_start = pos;
    let mut byte = *map
        .get(pos)
        .ok_or(PackError::Format("entry offset out of range"))?;
    pos += 1;
    let type_id = (byte >> 4) & 0b111;
    let mut size = u64::from(byte & 0b1111);
    let mut shift = 4;
    while byte & 0x80 != 0 {
        byte = *map
            .get(pos)
            .ok_or(PackError::Format("truncated entry size"))?;
        pos += 1;
        size |= u64::from(byte & 0x7f) << shift;
        shift += 7;
    }
    let kind = match type_id {
        1 => RawEntryKind::Plain(ObjectKind::Commit),
        2 => RawEntryKind::Plain(ObjectKind::Tree),
        3 => RawEntryKind::Plain(ObjectKind::Blob),
        4 => RawEntryKind::Plain(ObjectKind::Tag),
        6 => {
            byte = *map
                .get(pos)
                .ok_or(PackError::Format("truncated ofs-delta"))?;
            pos += 1;
            let mut dist = u64::from(byte & 0x7f);
            while byte & 0x80 != 0 {
                byte = *map
                    .get(pos)
                    .ok_or(PackError::Format("truncated ofs-delta"))?;
                pos += 1;
                dist = ((dist + 1) << 7) | u64::from(byte & 0x7f);
            }
            let base_at = offset
                .checked_sub(dist)
                .ok_or(PackError::Format("ofs-delta points before pack start"))?;
            RawEntryKind::Ofs { base_at }
        }
        7 => {
            let raw = algo.raw_len();
            let bytes = map
                .get(pos..pos + raw)
                .ok_or(PackError::Format("truncated ref-delta base id"))?;
            pos += raw;
            RawEntryKind::Ref {
                base: ObjectId::from_bytes(algo, bytes).unwrap(),
            }
        }
        _ => return Err(PackError::Format("invalid pack entry type")),
    };

    // inflate the zlib stream; ZlibDecoder::total_in() tells us how many
    // compressed bytes we consumed
    let mut decoder = ZlibDecoder::new(&map[pos..body_end as usize]);
    let mut payload = Vec::with_capacity(size as usize);
    decoder.read_to_end(&mut payload)?;
    if payload.len() as u64 != size {
        return Err(PackError::Format(
            "inflated size does not match entry header",
        ));
    }
    let compressed_len = decoder.total_in() as usize;
    let next_offset = (pos + compressed_len) as u64;
    if next_offset > body_end {
        return Err(PackError::Format("entry runs past pack body"));
    }

    let mut crc = Crc::new();
    crc.update(&map[header_start..next_offset as usize]);

    Ok(RawEntry {
        offset,
        next_offset,
        kind,
        payload,
        crc32: crc.sum(),
    })
}

/// Resolved (canonical kind + data) for one entry.
struct Resolved {
    kind: ObjectKind,
    data: Arc<Vec<u8>>,
}

fn resolve_all(entries: &[RawEntry]) -> Result<Vec<Resolved>, PackError> {
    let n = entries.len();
    let mut resolved: Vec<Option<Resolved>> = (0..n).map(|_| None).collect();
    let by_offset: HashMap<u64, usize> = entries
        .iter()
        .enumerate()
        .map(|(i, e)| (e.offset, i))
        .collect();

    // first pass: plain entries resolve directly
    for (i, e) in entries.iter().enumerate() {
        if let RawEntryKind::Plain(kind) = e.kind {
            resolved[i] = Some(Resolved {
                kind,
                data: Arc::new(e.payload.clone()),
            });
        }
    }

    // iterate until either everything is resolved or no progress (stuck
    // cycle / external base — both fatal for index-pack on a non-thin
    // pack)
    loop {
        let mut progressed = false;
        let mut still_unresolved = false;
        for (i, e) in entries.iter().enumerate() {
            if resolved[i].is_some() {
                continue;
            }
            let base_idx = match &e.kind {
                RawEntryKind::Plain(_) => unreachable!("plain entries resolved above"),
                RawEntryKind::Ofs { base_at } => *by_offset
                    .get(base_at)
                    .ok_or(PackError::Format("ofs-delta base offset not found"))?,
                RawEntryKind::Ref { base } => {
                    // find resolved entry with this oid
                    match resolved.iter().enumerate().find_map(|(j, r)| {
                        r.as_ref().and_then(|res| {
                            let oid = ObjectId::hash_object(base.algo(), res.kind, &res.data);
                            (oid == *base).then_some(j)
                        })
                    }) {
                        Some(j) => j,
                        None => {
                            still_unresolved = true;
                            continue;
                        }
                    }
                }
            };
            let Some(base_res) = &resolved[base_idx] else {
                still_unresolved = true;
                continue;
            };
            let applied = delta::apply(&base_res.data, &e.payload)?;
            resolved[i] = Some(Resolved {
                kind: base_res.kind,
                data: Arc::new(applied),
            });
            progressed = true;
        }
        if !still_unresolved {
            break;
        }
        if !progressed {
            return Err(PackError::Format(
                "delta resolution stuck (ref-delta base missing or cycle)",
            ));
        }
    }

    Ok(resolved
        .into_iter()
        .map(|r| r.expect("all entries resolved"))
        .collect())
}

fn verify_trailer(map: &[u8], algo: HashAlgo, trailer: &[u8]) -> Result<(), PackError> {
    use sha1::{Digest, Sha1};
    use sha2::Sha256;
    let body = &map[..map.len() - trailer.len()];
    let actual: Vec<u8> = match algo {
        HashAlgo::Sha1 => {
            let mut h = Sha1::new();
            h.update(body);
            h.finalize().to_vec()
        }
        HashAlgo::Sha256 => {
            let mut h = Sha256::new();
            h.update(body);
            h.finalize().to_vec()
        }
    };
    if actual != trailer {
        return Err(PackError::Format("pack trailer mismatch"));
    }
    Ok(())
}

fn read_be_u32(buf: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(buf[at..at + 4].try_into().unwrap())
}

/// V2 idx layout — same shape as [`crate::write::build_idx`], duplicated
/// here so we don't have to expose the writer's internals across modules
/// (and so this stays a self-contained read-and-index path).
fn build_idx(algo: HashAlgo, entries: &mut [(ObjectId, u64, u32)], trailer: &[u8]) -> Vec<u8> {
    use sha1::{Digest, Sha1};
    use sha2::Sha256;
    entries.sort_unstable_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));

    let mut out = Vec::with_capacity(8 + 256 * 4 + entries.len() * (algo.raw_len() + 8) + 64);
    out.extend_from_slice(&[0xff, b't', b'O', b'c']);
    out.extend_from_slice(&2u32.to_be_bytes());

    let mut fanout = [0u32; 256];
    for (oid, _, _) in entries.iter() {
        fanout[oid.as_bytes()[0] as usize] += 1;
    }
    let mut cumulative = 0u32;
    for count in fanout {
        cumulative += count;
        out.extend_from_slice(&cumulative.to_be_bytes());
    }
    for (oid, _, _) in entries.iter() {
        out.extend_from_slice(oid.as_bytes());
    }
    for (_, _, crc) in entries.iter() {
        out.extend_from_slice(&crc.to_be_bytes());
    }
    let mut large: Vec<u64> = Vec::new();
    for (_, offset, _) in entries.iter() {
        if *offset < (1 << 31) {
            out.extend_from_slice(&(*offset as u32).to_be_bytes());
        } else {
            out.extend_from_slice(&((1u32 << 31) | large.len() as u32).to_be_bytes());
            large.push(*offset);
        }
    }
    for offset in large {
        out.extend_from_slice(&offset.to_be_bytes());
    }
    out.extend_from_slice(trailer);

    let checksum: Vec<u8> = match algo {
        HashAlgo::Sha1 => {
            let mut h = Sha1::new();
            h.update(&out);
            h.finalize().to_vec()
        }
        HashAlgo::Sha256 => {
            let mut h = Sha256::new();
            h.update(&out);
            h.finalize().to_vec()
        }
    };
    out.extend_from_slice(&checksum);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{IndexedPack, PackWriter};
    use tempfile::TempDir;

    fn put(writer: &mut PackWriter, kind: ObjectKind, data: &[u8]) -> ObjectId {
        let oid = ObjectId::hash_object(HashAlgo::Sha1, kind, data);
        writer.add(oid, kind, data).unwrap();
        oid
    }

    /// A pack written by `PackWriter` (plain entries only) round-trips
    /// through `index_pack` — the produced idx is byte-equivalent to what
    /// `PackWriter::finish` wrote, and `IndexedPack::open` reads back the
    /// same objects.
    #[test]
    fn plain_pack_round_trips_through_index_pack() {
        let dir = TempDir::new().unwrap();
        let pack_dir = dir.path();

        let mut writer = PackWriter::create(pack_dir, HashAlgo::Sha1, 3).unwrap();
        let blob1 = put(&mut writer, ObjectKind::Blob, b"hello\n");
        let blob2 = put(&mut writer, ObjectKind::Blob, b"world\n");
        let tree = put(&mut writer, ObjectKind::Tree, b"");
        let written = writer.finish().unwrap();

        // delete the writer's idx and rebuild it from scratch via index_pack
        std::fs::remove_file(&written.idx_path).unwrap();
        let rebuilt = index_pack(&written.pack_path, HashAlgo::Sha1, false).unwrap();
        assert_eq!(rebuilt.objects, 3);
        assert_eq!(rebuilt.trailer, written.trailer);

        // opening as IndexedPack reads back every object
        let ip = IndexedPack::open(&rebuilt.pack_path, HashAlgo::Sha1).unwrap();
        for oid in [blob1, blob2, tree] {
            let obj = ip
                .read(&oid)
                .unwrap()
                .expect("object present after index_pack");
            // round-trip preserves identity
            let actual = ObjectId::hash_object(HashAlgo::Sha1, obj.kind, &obj.data);
            assert_eq!(actual, oid);
        }
    }

    /// A pack whose trailer doesn't match the body hash is rejected — a
    /// corrupt download must not produce a misleadingly-good idx.
    #[test]
    fn corrupt_trailer_is_rejected() {
        let dir = TempDir::new().unwrap();
        let mut writer = PackWriter::create(dir.path(), HashAlgo::Sha1, 1).unwrap();
        put(&mut writer, ObjectKind::Blob, b"x");
        let written = writer.finish().unwrap();

        // flip a byte in the trailer
        let mut bytes = std::fs::read(&written.pack_path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;
        std::fs::write(&written.pack_path, &bytes).unwrap();
        std::fs::remove_file(&written.idx_path).unwrap();

        let err = index_pack(&written.pack_path, HashAlgo::Sha1, false).unwrap_err();
        match err {
            PackError::Format(msg) => assert!(msg.contains("trailer"), "msg={msg}"),
            other => panic!("expected Format trailer error, got {other:?}"),
        }
    }
}
