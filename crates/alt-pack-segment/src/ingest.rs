//! Verifies a received pack once and produces the index that makes it a segment.

pub use crate::ingested::Ingested;
pub(crate) use crate::settled::Meta;
use crate::{
    Error, Kind, Led, Limits, Oid, SegmentSet,
    descend::Root,
    format::CommitRow,
    object_id,
    passes::Passes,
    plain, room,
    scan::{self, Form},
    settled::Resolved,
    told::Told,
};
use alt_git_pack::{
    Wait,
    walk::{Budget, Deltas, Of},
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering::Relaxed},
};

/// plain blobs above this size are hashed during the scan and not kept in memory
pub(crate) const KEEP: usize = 1 << 20;
/// Inflated bytes kept in memory between the scan and the later passes. Up to here a
/// pack is inflated once and hashed on every core; what lies beyond is hashed by the
/// scan itself and inflated again where a delta needs it, so that the memory a pack
/// takes does not grow with its size.
pub(crate) const HELD: u64 = 256 << 20;

pub(crate) fn digest(kind: Kind, data: &[u8]) -> (Oid, [u8; 32]) {
    (
        object_id(kind, data),
        *alt_hash::blake3::hash(data).as_bytes(),
    )
}

/// `pack` is the complete received pack including its trailer
pub fn ingest(pack: &[u8], existing: &SegmentSet, limits: &Limits) -> Result<Ingested, Error> {
    verified(pack, (existing, None), limits, HELD, None)
}

/// [`ingest`] for as long as it is waited for: once `given_up` is set, the work
/// ends at the next entry or delta it comes to.
pub fn ingest_while_waited_for(
    pack: &[u8],
    existing: &SegmentSet,
    limits: &Limits,
    given_up: &Arc<AtomicBool>,
) -> Result<Ingested, Error> {
    verified(pack, (existing, None), limits, HELD, Some(given_up))
}

/// [`ingest`] of a pack sent by one who is led to `led` of what `existing`
/// holds: an object beyond that is none the pack may refer to, as the object
/// of a tree or a commit or as the base of a delta, unless the pack carries it.
pub fn ingest_led(
    pack: &[u8],
    (existing, led): (&SegmentSet, &Led),
    limits: &Limits,
    given_up: Option<&Arc<AtomicBool>>,
) -> Result<Ingested, Error> {
    verified(pack, (existing, Some(led)), limits, HELD, given_up)
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
    verified(pack, (existing, None), limits, held, None)
}

fn verified(
    pack: &[u8],
    (existing, led): (&SegmentSet, Option<&Led>),
    limits: &Limits,
    held: u64,
    given_up: Option<&Arc<AtomicBool>>,
) -> Result<Ingested, Error> {
    let wait = Wait::new(limits.memory_by, given_up.map(|given_up| &**given_up));
    let going = || match wait.gone {
        Some(gone) if gone.load(Relaxed) => Err(Error::GivenUp),
        _ => Ok(()),
    };
    let said = pack.get(8..12).map_or(0, |count| {
        u32::from_be_bytes(count.try_into().expect("four bytes")) as usize
    });
    let told = Told::new((existing, led), said, limits.parents_beside);
    let mut memory = room::scanning(pack.len() as u64, said as u64, limits, (held, wait))?;
    let mut unkept: Vec<(usize, Meta)> = Vec::new();
    let mut commits: Vec<CommitRow> = Vec::new();
    let mut refused = None;
    let (trailer, scanned) = alt_threads::join(
        || -> Oid { alt_hash::sha1(&pack[..pack.len().saturating_sub(20)]) },
        || {
            // what the scan does not keep is hashed and taken in as it goes by
            scan::scan(pack, limits, &mut memory, going, |index, kind, data| {
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
                way: Vec::new(),
            });
        }
        metas[index] = Some(meta);
    }
    // thin pack: bases that live in earlier segments, numbered behind the entries
    let (mut outside, mut below, mut bases) = (Vec::new(), Vec::new(), Vec::new());
    let mut outer = Vec::new();
    for named in deltas.by_object() {
        let base = named[0].0;
        if led.is_none_or(|led| led.to(&base))
            && let Some(record) = existing.base_record(&base)?
        {
            let entry = u32::try_from(entries.len() + outside.len());
            outer.push(Root {
                entry: entry.map_err(|_| Error::Limit("objects"))?,
                oid: base,
                kind: record.kind,
                // the chain goes on below the base, in the segment that holds it
                depth: record.depth,
                way: Vec::new(),
            });
            outside.push(base);
            bases.push((base, record.kind));
            below.push(record.depth);
            scanned.largest = scanned.largest.max(record.size);
        }
    }
    let mut budget = Budget::new(
        (limits.object_bytes, limits.inflated_bytes),
        (limits.base_bytes, limits.spill_bytes),
        u32::from(limits.delta_depth) + 1,
        scanned.inflated,
    );
    let memory = memory.resolving(limits, &mut scanned)?;
    memory.told_to(&mut budget, limits.files);
    let passes = Passes {
        pack,
        entries: &scanned.entries,
        existing,
        told: &told,
        deltas: &deltas,
        budget: &budget,
        outside: &outside,
        below: &below,
        walks: memory.walks,
        depth: limits.delta_depth,
    };
    let work = passes.resolve((roots, outer), &mut metas, &mut commits)?;
    let resolved = metas.into_iter().zip(&scanned.entries).map(|(meta, raw)| {
        meta.ok_or(match raw.form {
            Form::Reference(base) => Error::MissingBase(base),
            _ => Error::Format("delta without a resolvable base"),
        })
    });
    let metas: Vec<Meta> = resolved.collect::<Result<_, Error>>()?;
    // an object the pack carries twice told of its commit twice
    commits.sort_unstable_by_key(|commit| commit.oid);
    commits.dedup_by(|a, b| a.oid == b.oid);
    // what the walks had is given back; what is kept of the pack stays taken
    let mut entries = scanned.entries;
    for raw in &mut entries {
        raw.data = None;
    }
    let resolved = Resolved {
        _held: memory.keeping(&entries),
        metas,
        entries,
        commits,
        depth: limits.delta_depth,
    };
    let mut flagged = std::mem::take(&mut *told.flagged.lock().expect("flagged"));
    flagged.sort_unstable_by_key(|(tree, _)| *tree);
    flagged.dedup_by_key(|(tree, _)| *tree);
    // every reference resolves inside the pack or in the segments already present
    let mut leans = Vec::new();
    let settled = resolved.settle(existing, |carried| {
        let of = |from: u32| resolved.metas[from as usize].oid;
        leans = told.wanted.check(carried, of)?;
        Ok(())
    })?;
    leans.extend(bases);
    leans.sort_unstable_by_key(|(oid, _)| *oid);
    leans.dedup_by_key(|(oid, _)| *oid);
    Ok(Ingested {
        records: settled.records,
        commits: settled.commits,
        leans,
        flagged,
        flags: settled.flags,
        pack_len: pack.len() as u64,
        work,
        resolved,
    })
}
