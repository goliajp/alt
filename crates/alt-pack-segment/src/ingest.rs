//! Verifies a received pack once and produces the index that makes it a segment.

use crate::{
    Error, FLAG_DUPLICATES, FLAG_OVERLAP, Kind, Limits, Oid, Record, SegmentSet,
    carried::Carried,
    descend::{self, Root, Shared},
    format::{self, CommitRow, Delta, NO_BASE},
    object_id, plain, room,
    scan::{self, Form},
    told::Told,
};
use alt_git_pack::{
    IndexWork,
    walk::{Budget, Deltas, Of},
};
use std::io::Write;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering::Relaxed},
};

/// plain blobs above this size are hashed during the scan and not kept in memory
const KEEP: usize = 1 << 20;
/// Inflated bytes kept in memory between the scan and the later passes. Up to here a
/// pack is inflated once and hashed on every core; what lies beyond is hashed by the
/// scan itself and inflated again where a delta needs it, so that the memory a pack
/// takes does not grow with its size.
pub(crate) const HELD: u64 = 256 << 20;

pub(crate) struct Meta {
    pub oid: Oid,
    pub blake3: [u8; 32],
    pub kind: Kind,
    pub size: u64,
    pub depth: u16,
    /// scan index of the base inside this pack
    pub base: Option<usize>,
    pub outside: bool,
}

impl Meta {
    pub(crate) fn plain(oid: Oid, blake3: [u8; 32], kind: Kind, size: u64) -> Self {
        Meta {
            oid,
            blake3,
            kind,
            size,
            depth: 0,
            base: None,
            outside: false,
        }
    }
}

pub struct Ingested {
    records: Vec<Record>,
    commits: Vec<CommitRow>,
    flags: u32,
    pack_len: u64,
    work: IndexWork,
}

impl Ingested {
    /// The work resolving the deltas took, and the most their bases took at once.
    pub fn work(&self) -> IndexWork {
        self.work
    }

    pub fn objects(&self) -> usize {
        self.records.len()
    }

    pub fn flags(&self) -> u32 {
        self.flags
    }

    pub fn contains(&self, oid: &Oid) -> bool {
        self.find(oid).is_some()
    }

    fn find(&self, oid: &Oid) -> Option<usize> {
        self.records
            .binary_search_by(|record| record.oid.cmp(oid))
            .ok()
    }

    pub fn kind(&self, oid: &Oid) -> Option<Kind> {
        self.find(oid).map(|index| self.records[index].kind)
    }

    /// parents and tree of a commit carried by this pack
    pub fn commit(&self, oid: &Oid) -> Option<(&Oid, &[Oid])> {
        self.commits
            .binary_search_by(|commit| commit.oid.cmp(oid))
            .ok()
            .map(|index| {
                (
                    &self.commits[index].tree,
                    self.commits[index].parents.as_slice(),
                )
            })
    }

    /// appends the index; the caller has already written the `pack_len` pack bytes
    pub fn write_index(&self, out: &mut impl Write) -> Result<(), Error> {
        format::write_index(&self.records, &self.commits, self.flags, self.pack_len, out)
    }
}

pub(crate) fn digest(kind: Kind, data: &[u8]) -> (Oid, [u8; 32]) {
    (
        object_id(kind, data),
        *alt_hash::blake3::hash(data).as_bytes(),
    )
}

/// `pack` is the complete received pack including its trailer
pub fn ingest(pack: &[u8], existing: &SegmentSet, limits: &Limits) -> Result<Ingested, Error> {
    verified(pack, existing, limits, HELD, None)
}

/// [`ingest`] for as long as it is waited for: once `given_up` is set, the work
/// ends at the next entry or delta it comes to.
pub fn ingest_while_waited_for(
    pack: &[u8],
    existing: &SegmentSet,
    limits: &Limits,
    given_up: &Arc<AtomicBool>,
) -> Result<Ingested, Error> {
    verified(pack, existing, limits, HELD, Some(given_up))
}

/// `ingest` with a chosen bound on the inflated bytes kept in memory; the result does
/// not depend on it
#[doc(hidden)]
pub fn ingest_holding(
    pack: &[u8],
    existing: &SegmentSet,
    limits: &Limits,
    held: u64,
) -> Result<Ingested, Error> {
    verified(pack, existing, limits, held, None)
}

fn verified(
    pack: &[u8],
    existing: &SegmentSet,
    limits: &Limits,
    held: u64,
    given_up: Option<&Arc<AtomicBool>>,
) -> Result<Ingested, Error> {
    let going = || match given_up {
        Some(given_up) if given_up.load(Relaxed) => Err(Error::GivenUp),
        _ => Ok(()),
    };
    let said = pack.get(8..12).map_or(0, |count| {
        u32::from_be_bytes(count.try_into().expect("four bytes")) as usize
    });
    let told = Told::new(existing, said, limits.parents_beside);
    // an entry is inflated whole to be scanned, beside what the scan keeps
    let memory = room::scanning(pack.len() as u64, said as u64, limits, held)?;
    let mut unkept: Vec<(usize, Meta)> = Vec::new();
    let mut commits: Vec<CommitRow> = Vec::new();
    let mut refused = None;
    let (trailer, scanned) = alt_threads::join(
        || -> Oid { alt_hash::sha1(&pack[..pack.len().saturating_sub(20)]) },
        || {
            // what the scan does not keep is hashed and taken in as it goes by
            scan::scan(pack, limits, KEEP, held, &going, |index, kind, data| {
                let (oid, blake3) = digest(kind, data);
                match told.object(index as u32, &oid, kind, data) {
                    Ok(row) => commits.extend(row),
                    Err(error) => refused = refused.take().or(Some(error)),
                }
                let size = data.len() as u64;
                unkept.push((index, Meta::plain(oid, blake3, kind, size)));
            })
        },
    );
    let mut scanned = scanned?;
    if trailer[..] != pack[scanned.body_end..] {
        return Err(Error::Format("pack checksum"));
    }
    refused.map_or(Ok(()), Err)?;
    let entries = &scanned.entries;
    let of = entries.iter().map(|raw| match raw.form {
        Form::Plain(_) => Ok(Of::Nothing),
        Form::Offset(base) => entries
            .binary_search_by_key(&base, |raw| raw.offset)
            .map(|base| Of::Entry(base as u32))
            .map_err(|_| Error::Format("delta base offset")),
        Form::Reference(base) => Ok(Of::Object(base)),
    });
    let deltas = Deltas::new(of.collect::<Result<Vec<_>, Error>>()?);

    let kept = plain::hashed(entries, &told)?;
    commits.extend(kept.commits);
    let mut metas: Vec<Option<Meta>> = (0..entries.len()).map(|_| None).collect();
    let mut roots = Vec::new();
    for (index, meta) in kept.metas.into_iter().chain(unkept) {
        if deltas.any(index as u32, &meta.oid) {
            roots.push(Root {
                entry: index as u32,
                oid: meta.oid,
                kind: meta.kind,
                depth: 0,
            });
        }
        metas[index] = Some(meta);
    }
    // thin pack: bases that live in earlier segments, numbered behind the entries
    let mut outside = Vec::new();
    let mut outer = Vec::new();
    for named in deltas.by_object() {
        let base = named[0].0;
        if let Some(record) = existing.record(&base)? {
            let entry = u32::try_from(entries.len() + outside.len());
            outer.push(Root {
                entry: entry.map_err(|_| Error::Limit("objects"))?,
                oid: base,
                kind: record.kind,
                // the chain goes on below the base, in the segment that holds it
                depth: record.depth,
            });
            outside.push(base);
            scanned.largest = scanned.largest.max(record.size);
        }
    }
    let mut budget = Budget::new(
        (limits.object_bytes, limits.inflated_bytes),
        (limits.base_bytes, limits.spill_bytes),
        u32::from(limits.delta_depth),
        scanned.inflated,
    );
    (budget.given_up, budget.files) = (given_up.cloned(), limits.files);
    let (walks, _memory) = room::resolving(limits, &mut scanned, memory)?;
    let mut work = IndexWork::default();
    let taken: Vec<AtomicBool> = (scanned.entries.iter())
        .map(|_| AtomicBool::new(false))
        .collect();
    for pass in [roots, outer] {
        let shared = Shared {
            pack,
            entries: &scanned.entries,
            existing,
            told: &told,
            deltas: &deltas,
            budget: &budget,
            outside: &outside,
            taken: &taken,
        };
        let made = descend::below(&shared, pass, walks)?;
        commits.extend(made.commits);
        work.inflated += made.work.inflated;
        work.applied += made.work.applied;
        work.hashed += made.work.hashed;
        for (index, meta) in made.metas {
            metas[index].get_or_insert(meta);
        }
    }
    (work.held, work.spilled) = budget.most();
    let entries = &scanned.entries;
    let resolved = metas.into_iter().zip(entries).map(|(meta, raw)| {
        meta.ok_or(match raw.form {
            Form::Reference(base) => Error::MissingBase(base),
            _ => Error::Format("delta without a resolvable base"),
        })
    });
    let metas: Vec<Meta> = resolved.collect::<Result<_, Error>>()?;

    // records sorted by oid, first occurrence wins
    let mut order: Vec<usize> = (0..metas.len()).collect();
    order.sort_unstable_by(|a, b| metas[*a].oid.cmp(&metas[*b].oid).then(a.cmp(b)));
    let mut flags = 0;
    let mut kept: Vec<usize> = Vec::with_capacity(order.len());
    let mut position = vec![0u32; metas.len()];
    for index in order {
        match kept.last() {
            Some(last) if metas[*last].oid == metas[index].oid => flags |= FLAG_DUPLICATES,
            _ => kept.push(index),
        }
        position[index] = (kept.len() - 1) as u32;
    }
    // every reference resolves inside the pack or in the segments already present
    let carried = Carried::new(
        kept.iter()
            .map(|index| (metas[*index].oid, metas[*index].kind)),
    );
    told.wanted
        .check(&carried, |from| metas[from as usize].oid)?;
    if alt_threads::any(&kept, 1024, |index| {
        existing.locate(&metas[*index].oid).is_some()
    }) {
        flags |= FLAG_OVERLAP;
    }

    // an object the pack carries twice told of its commit twice
    commits.sort_unstable_by_key(|commit| commit.oid);
    commits.dedup_by(|a, b| a.oid == b.oid);
    crate::generation::assign(&mut commits, existing)?;
    let records = kept
        .iter()
        .map(|index| {
            let (meta, raw) = (&metas[*index], &entries[*index]);
            Record {
                oid: meta.oid,
                blake3: meta.blake3,
                offset: raw.offset,
                size: meta.size,
                crc: raw.crc,
                len: raw.len,
                base: meta.base.map_or(NO_BASE, |base| position[base]),
                kind: meta.kind,
                delta: match (raw.form, meta.outside) {
                    (Form::Plain(_), _) => Delta::Plain,
                    (_, true) => Delta::Outside,
                    (_, false) => Delta::Inside,
                },
                depth: meta.depth,
            }
        })
        .collect();
    Ok(Ingested {
        records,
        commits,
        flags,
        pack_len: pack.len() as u64,
        work,
    })
}
