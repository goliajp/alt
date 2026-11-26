//! A pack once it is verified: what its segment is told of, and what of the
//! segments before it it leans on.

use crate::{
    Error, Kind, Oid, Record, SegmentSet,
    format::{self, CommitRow},
    settled::Resolved,
};
use alt_git_pack::IndexWork;
use std::io::Write;

pub struct Ingested {
    pub(crate) records: Vec<Record>,
    pub(crate) commits: Vec<CommitRow>,
    /// the objects of the segments the pack leans on, each once, in the order
    /// of the ids: what it names and does not carry, and the bases of its
    /// deltas there
    pub(crate) leans: Vec<(Oid, Kind)>,
    /// `leans` is all the pack leans on: it leaned on no more than was kept
    pub(crate) leans_whole: bool,
    /// the trees `git fsck` looks at again, each once, in the order of the ids
    pub(crate) flagged: Vec<(Oid, alt_git_fsck::Flagged<Oid>)>,
    pub(crate) flags: u32,
    pub(crate) pack_len: u64,
    pub(crate) work: IndexWork,
    pub(crate) resolved: Resolved,
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

    /// The objects of the segments the pack was verified behind that it leans
    /// on: those it names and does not carry, and the bases of its deltas
    /// there, each once with its kind, in the order of the ids.
    pub fn leans(&self) -> &[(Oid, Kind)] {
        &self.leans
    }

    /// Whether [`Ingested::leans`] is all the pack leans on: what it leans on
    /// is kept as many as it has entries, and some hundred thousand more.
    pub fn leans_whole(&self) -> bool {
        self.leans_whole
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

    /// Tells the segment of the pack anew, for being published behind
    /// `existing` where it was verified behind other segments: how deep its
    /// deltas of earlier segments lie, which of two ways an object is read
    /// by, and whether it brings an object again are told of the segments
    /// before it. No entry is inflated for this and no object hashed: what
    /// each entry makes does not depend on the segments.
    pub fn behind(&mut self, existing: &SegmentSet) -> Result<(), Error> {
        let settled = self.resolved.settle(existing, |_| Ok(()))?;
        self.records = settled.records;
        self.commits = settled.commits;
        self.flags = settled.flags;
        Ok(())
    }

    /// appends the index; the caller has already written the `pack_len` pack bytes
    pub fn write_index(&self, out: &mut impl Write) -> Result<(), Error> {
        format::write_index(&self.records, &self.commits, self.flags, self.pack_len, out)
    }
}
