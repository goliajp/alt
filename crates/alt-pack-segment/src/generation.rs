//! Generation numbers of the commits a new segment carries.

use crate::{Error, Oid, SegmentSet, format::CommitRow};
use std::collections::HashMap;

/// Sets each commit's generation to one more than the largest among its parents,
/// which are other rows of `commits` or commits of `existing`.
pub(crate) fn assign(commits: &mut [CommitRow], existing: &SegmentSet) -> Result<(), Error> {
    let slot: HashMap<Oid, usize> = commits
        .iter()
        .enumerate()
        .map(|(at, commit)| (commit.oid, at))
        .collect();
    let mut generation = vec![0u32; commits.len()];
    for start in 0..commits.len() {
        let mut stack = vec![start];
        while let Some(at) = stack.last().copied() {
            if generation[at] != 0 {
                stack.pop();
                continue;
            }
            let mut highest = 0;
            let mut waiting = false;
            for parent in &commits[at].parents {
                match slot.get(parent) {
                    Some(parent_at) if generation[*parent_at] == 0 => {
                        if stack.contains(parent_at) {
                            return Err(Error::Format("commit ancestry cycle"));
                        }
                        stack.push(*parent_at);
                        waiting = true;
                    }
                    Some(parent_at) => highest = highest.max(generation[*parent_at]),
                    None => {
                        highest = highest.max(
                            existing
                                .commit(parent)
                                .map_or(0, |commit| commit.generation),
                        )
                    }
                }
            }
            if !waiting {
                generation[at] = highest + 1;
                stack.pop();
            }
        }
    }
    for (commit, generation) in commits.iter_mut().zip(generation) {
        commit.generation = generation;
    }
    Ok(())
}
