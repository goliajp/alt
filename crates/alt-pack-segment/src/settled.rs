//! What the entries of a pack make, kept once every delta is resolved, and
//! what the segment of the pack is told to be behind given segments.
//!
//! What an entry makes is the same behind any segments that hold its base.
//! What is told of the object is not: how deep a delta of an earlier segment
//! lies, which of two ways an object is read by, whether the pack brings an
//! object again. A pack that is published behind other segments than it was
//! verified behind is told anew from what is kept here, with nothing
//! inflated and nothing hashed.

use crate::{
    Error, FLAG_DUPLICATES, FLAG_OVERLAP, Kind, Oid, Record, SegmentSet,
    carried::Carried,
    format::{CommitRow, Delta},
    scan::{Form, Raw},
};

/// what an entry of a pack makes
pub(crate) struct Meta {
    pub oid: Oid,
    pub blake3: [u8; 32],
    pub kind: Kind,
    pub size: u64,
    /// scan index of the entry of this pack the delta was resolved on, where
    /// it was resolved on one
    pub base: Option<usize>,
}

impl Meta {
    pub(crate) fn plain(oid: Oid, blake3: [u8; 32], kind: Kind, size: u64) -> Self {
        Meta {
            oid,
            blake3,
            kind,
            size,
            base: None,
        }
    }
}

pub(crate) struct Resolved {
    /// the memory `metas` and `entries` are counted in, where packs have a bound
    pub _held: Option<alt_git_pack::Taken<'static>>,
    pub metas: Vec<Meta>,
    /// as they were scanned, without what they inflate to
    pub entries: Vec<Raw>,
    /// each commit once, in the order of the ids, before it is told how far it
    /// lies from a commit without parents
    pub commits: Vec<CommitRow>,
    /// the most deltas there may be below an object
    pub depth: u16,
}

pub(crate) struct Settled {
    pub records: Vec<Record>,
    pub commits: Vec<CommitRow>,
    pub flags: u32,
}

impl Resolved {
    /// The records of the segment behind `existing`. `carried` is given the
    /// objects of the pack once it is known which entry each is read from.
    pub(crate) fn settle(
        &self,
        existing: &SegmentSet,
        carried: impl FnOnce(&Carried) -> Result<(), Error>,
    ) -> Result<Settled, Error> {
        let (metas, entries) = (&self.metas, &self.entries);
        // the bases that live in earlier segments, with the deltas below each there
        let mut named: Vec<Oid> = (entries.iter())
            .filter_map(|raw| match raw.form {
                Form::Reference(base) => Some(base),
                _ => None,
            })
            .collect();
        named.sort_unstable();
        named.dedup();
        let mut earlier = Vec::new();
        for base in named {
            if let Some(record) = existing.base_record(&base)? {
                earlier.push((base, record.depth));
            }
        }
        let chosen = crate::chosen::choose(metas, |at| entries[at].form, &earlier, self.depth)?;
        let kept = &chosen.kept;
        let mut flags = if chosen.duplicates {
            FLAG_DUPLICATES
        } else {
            0
        };
        carried(&Carried::new(
            kept.iter()
                .map(|index| (metas[*index].oid, metas[*index].kind)),
        ))?;
        if alt_threads::any(kept, 1024, |index| {
            existing.locate(&metas[*index].oid).is_some()
        }) {
            flags |= FLAG_OVERLAP;
        }
        let mut commits: Vec<CommitRow> = (self.commits.iter())
            .map(|commit| CommitRow {
                oid: commit.oid,
                tree: commit.tree,
                generation: commit.generation,
                parents: commit.parents.clone(),
            })
            .collect();
        crate::generation::assign(&mut commits, existing)?;
        let records = kept
            .iter()
            .enumerate()
            .map(|(at, index)| {
                let (meta, raw) = (&metas[*index], &entries[*index]);
                Record {
                    oid: meta.oid,
                    blake3: meta.blake3,
                    offset: raw.offset,
                    size: meta.size,
                    crc: raw.crc,
                    len: raw.len,
                    base: chosen.base[at],
                    kind: meta.kind,
                    delta: match (raw.form, chosen.outside[at]) {
                        (Form::Plain(_), _) => Delta::Plain,
                        (_, true) => Delta::Outside,
                        (_, false) => Delta::Inside,
                    },
                    depth: chosen.depth[at],
                }
            })
            .collect();
        Ok(Settled {
            records,
            commits,
            flags,
        })
    }
}
