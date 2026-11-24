//! Resolves the deltas of a received pack below their bases.

use crate::{
    Error, Kind, Oid, SegmentSet,
    format::CommitRow,
    ingest::{Meta, digest},
    scan::{self, Raw},
    told::Told,
};
use alt_git_pack::{
    IndexWork,
    walk::{Budget, Deltas, Objects, Walk},
};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

pub(crate) struct Shared<'a> {
    pub pack: &'a [u8],
    pub entries: &'a [Raw],
    pub existing: &'a SegmentSet,
    pub told: &'a Told<'a>,
    pub deltas: &'a Deltas<Oid>,
    pub budget: &'a Budget,
    /// the bases that earlier segments hold: the numbers behind the last entry of
    /// the pack are theirs
    pub outside: &'a [Oid],
    /// By entry, whether a walk has taken the delta to resolve. Several deltas
    /// may make one object, and each of them comes to every delta of that
    /// object: the first that does resolves it, in whichever walk and pass.
    pub taken: &'a [AtomicBool],
}

/// An object no delta of this pack made, which deltas are made of.
pub(crate) struct Root {
    pub entry: u32,
    pub oid: Oid,
    pub kind: Kind,
    /// deltas below a plain entry, in the segment that holds it
    pub depth: u16,
}

#[derive(Default)]
pub(crate) struct Made {
    pub metas: Vec<(usize, Meta)>,
    pub commits: Vec<CommitRow>,
    pub work: IndexWork,
}

struct Verified<'s> {
    shared: &'s Shared<'s>,
    /// of the root the walk is below
    kind: Kind,
    made: Made,
}

impl Objects for Verified<'_> {
    type Id = Oid;
    type Error = Error;

    /// A base is taken when a delta of it is come to, not before: of a plain entry
    /// the scan kept, the bytes it kept; of any other, read anew.
    fn payload<'a>(&'a mut self, entry: u32, scratch: &'a mut Vec<u8>) -> Result<&'a [u8], Error> {
        let shared = self.shared;
        let Some(raw) = shared.entries.get(entry as usize) else {
            let base = &shared.outside[entry as usize - shared.entries.len()];
            let read = shared.existing.read(base);
            *scratch = read.map_err(|_| Error::MissingBase(*base))?.1;
            return Ok(scratch);
        };
        if let Some(data) = &raw.data {
            return Ok(data);
        }
        let start = raw.offset as usize + raw.header as usize;
        *scratch = scan::inflate(&shared.pack[start..], raw.size as usize)?.0;
        Ok(scratch)
    }

    fn reached(&self, delta: u32) -> bool {
        self.shared.taken[delta as usize].swap(true, Relaxed)
    }

    fn made(&mut self, delta: u32, of: u32, depth: u32, object: &[u8]) -> Result<Oid, Error> {
        let (shared, kind) = (self.shared, self.kind);
        let (oid, blake3) = digest(kind, object);
        let row = shared.told.object(delta, &oid, kind, object)?;
        self.made.commits.extend(row);
        let base = ((of as usize) < shared.entries.len()).then_some(of as usize);
        let meta = Meta {
            oid,
            blake3,
            kind,
            size: object.len() as u64,
            depth: depth as u16,
            base,
            outside: base.is_none(),
        };
        self.made.metas.push((delta as usize, meta));
        Ok(oid)
    }
}

/// Resolves every delta below `roots`, below no more of them at once than `walks`
/// where that is told.
pub(crate) fn below(
    shared: &Shared<'_>,
    roots: Vec<Root>,
    walks: Option<usize>,
) -> Result<Made, Error> {
    let groups: Vec<Vec<Root>> = match walks {
        None => roots.into_iter().map(|root| vec![root]).collect(),
        Some(walks) => {
            let mut groups: Vec<Vec<Root>> = (0..walks.max(1)).map(|_| Vec::new()).collect();
            for (at, root) in roots.into_iter().enumerate() {
                groups[at % walks.max(1)].push(root);
            }
            groups
        }
    };
    let parts = alt_threads::map_owned(groups, 1, |roots| -> Result<Made, Error> {
        let verified = Verified {
            shared,
            kind: Kind::Blob,
            made: Made::default(),
        };
        let mut walk = Walk::new(verified, shared.deltas, shared.budget);
        for root in roots {
            walk.objects.kind = root.kind;
            walk.below(root.entry, &root.oid, u32::from(root.depth))?;
        }
        let mut made = walk.objects.made;
        made.work = walk.work;
        Ok(made)
    });
    let mut all = Made::default();
    for part in parts {
        let part = part?;
        all.metas.extend(part.metas);
        all.commits.extend(part.commits);
        all.work.inflated += part.work.inflated;
        all.work.applied += part.work.applied;
        all.work.hashed += part.work.hashed;
    }
    Ok(all)
}
