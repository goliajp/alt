//! Which of the commits a client says it has the refs it may read lead to.
//!
//! A segment keeps what was pushed, also what no ref names any more: the tip of a
//! branch that was deleted, the history a forced push replaced. That a client
//! names such a commit is no reason to tell it the commit is here, nor to leave
//! out of what it is sent what only that commit reaches.

use crate::{
    Error, Kind, Oid, OidMap, OidSet, SegmentSet,
    object::{self, Parsed},
};
use std::collections::BinaryHeap;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

/// how many commits a walk takes between two looks at whether it is waited for
const LOOKED_EVERY: usize = 4096;

impl SegmentSet {
    /// Whether `tips` lead to every one of `sought`, as the commits tell it:
    /// a tag among the tips is gone through to what it names, and one of
    /// `sought` that is no commit is led to by none.
    pub fn lead_to_all(&self, tips: &[Oid], sought: &[Oid], steps: usize) -> Result<bool, Error> {
        self.lead_to_all_while(tips, sought, &mut { steps }, None)
    }

    /// The same, taking the commits it goes through out of `steps`, which
    /// several of these share, and ending with [`Error::GivenUp`] once `gone`
    /// is set.
    pub fn lead_to_all_while(
        &self,
        tips: &[Oid],
        sought: &[Oid],
        steps: &mut usize,
        gone: Option<&AtomicBool>,
    ) -> Result<bool, Error> {
        let mut commits = Vec::with_capacity(tips.len());
        for tip in tips {
            let mut oid = *tip;
            while self.commit(&oid).is_none() && self.kind(&oid)? == Some(Kind::Tag) {
                let (kind, data) = self.read(&oid)?;
                let Parsed::Tag { object, .. } = object::parse(kind, &data)? else {
                    unreachable!()
                };
                oid = object;
            }
            commits.push(oid);
        }
        Ok(self.reached_while(&commits, sought, steps, gone)?.len() == sought.len())
    }

    /// Those of `said` that are commits one of `tips` reaches, in the order they
    /// were said. A commit has a higher generation than every commit it reaches,
    /// so nothing below the lowest generation looked for is gone to; the walk
    /// takes `steps` commits at the most, and ends with [`Error::Limit`] beyond.
    pub fn reached(&self, tips: &[Oid], said: &[Oid], steps: usize) -> Result<Vec<Oid>, Error> {
        self.reached_while(tips, said, &mut { steps }, None)
    }

    fn reached_while(
        &self,
        tips: &[Oid],
        said: &[Oid],
        steps: &mut usize,
        gone: Option<&AtomicBool>,
    ) -> Result<Vec<Oid>, Error> {
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
            if taken % LOOKED_EVERY == 0 && gone.is_some_and(|gone| gone.load(Relaxed)) {
                return Err(Error::GivenUp);
            }
            taken += 1;
            *steps = (steps.checked_sub(1)).ok_or(Error::Limit("commits of a fetch"))?;
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
