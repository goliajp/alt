//! Which of the commits a client says it has the refs it may read lead to.
//!
//! A segment keeps what was pushed, also what no ref names any more: the tip of a
//! branch that was deleted, the history a forced push replaced. That a client
//! names such a commit is no reason to tell it the commit is here, nor to leave
//! out of what it is sent what only that commit reaches.

use crate::{Error, Oid, OidMap, OidSet, SegmentSet};
use std::collections::BinaryHeap;

impl SegmentSet {
    /// Those of `said` that are commits one of `tips` reaches, in the order they
    /// were said. A commit has a higher generation than every commit it reaches,
    /// so nothing below the lowest generation looked for is gone to; the walk
    /// takes `steps` commits at the most, and ends with [`Error::Limit`] beyond.
    pub fn reached(&self, tips: &[Oid], said: &[Oid], steps: usize) -> Result<Vec<Oid>, Error> {
        let sought: OidMap<u32> = (said.iter())
            .filter_map(|oid| Some((*oid, self.commit(oid)?.generation)))
            .collect();
        let Some(lowest) = sought.values().copied().min() else {
            return Ok(Vec::new());
        };
        let mut seen = OidSet::default();
        let mut heap = BinaryHeap::new();
        let mut come_to = |oid: &Oid, heap: &mut BinaryHeap<(u32, Oid)>| {
            if let Some(commit) = self
                .commit(oid)
                .filter(|commit| commit.generation >= lowest)
                && seen.insert(*oid)
            {
                heap.push((commit.generation, *oid));
            }
        };
        for tip in tips {
            come_to(tip, &mut heap);
        }
        let mut found = OidSet::default();
        let mut taken = 0usize;
        while found.len() < sought.len()
            && let Some((_, oid)) = heap.pop()
        {
            taken += 1;
            if taken > steps {
                return Err(Error::Limit("commits of a fetch"));
            }
            if sought.contains_key(&oid) {
                found.insert(oid);
            }
            let commit = self.commit(&oid).ok_or(Error::Corrupt("commit graph"))?;
            for parent in &commit.parents {
                come_to(parent, &mut heap);
            }
        }
        Ok((said.iter().filter(|oid| found.contains(*oid)).copied()).collect())
    }
}
