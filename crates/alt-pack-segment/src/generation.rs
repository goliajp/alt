//! Generation numbers of the commits a new segment carries.

use crate::{Error, OidMap, SegmentSet, format::CommitRow};

/// Sets each commit's generation to one more than the largest among its parents,
/// which are other rows of `commits` or commits of `existing`.
pub(crate) fn assign(commits: &mut [CommitRow], existing: &SegmentSet) -> Result<(), Error> {
    let slot: OidMap<usize> = commits
        .iter()
        .enumerate()
        .map(|(at, commit)| (commit.oid, at))
        .collect();
    let mut generation = vec![0u32; commits.len()];
    // a history is as deep as it is long: whether a commit is on the stack is not
    // found by going through the stack
    let mut stacked = vec![false; commits.len()];
    for start in 0..commits.len() {
        let mut stack = vec![start];
        while let Some(at) = stack.last().copied() {
            if generation[at] != 0 {
                stack.pop();
                continue;
            }
            stacked[at] = true;
            let mut highest = 0;
            let mut waiting = false;
            for parent in &commits[at].parents {
                match slot.get(parent) {
                    Some(parent_at) if generation[*parent_at] == 0 => {
                        if stacked[*parent_at] {
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
                stacked[at] = false;
                stack.pop();
            }
        }
    }
    for (commit, generation) in commits.iter_mut().zip(generation) {
        commit.generation = generation;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(oid: u8, parents: &[u8]) -> CommitRow {
        CommitRow {
            oid: [oid; 20],
            tree: [0; 20],
            generation: 0,
            parents: parents.iter().map(|parent| [*parent; 20]).collect(),
        }
    }

    #[test]
    fn a_parent_reached_twice_is_no_cycle_and_a_commit_below_itself_is_one() {
        let none = SegmentSet::default();
        let generations = |mut commits: Vec<CommitRow>| {
            assign(&mut commits, &none).unwrap();
            let all = commits.iter().map(|commit| commit.generation);
            all.collect::<Vec<u32>>()
        };
        // A merge of a branch into the commit the branch began at: the second
        // parent has the first one below it. The merge comes first, so both of its
        // parents wait when the second is gone into.
        let merge = vec![row(1, &[2, 3]), row(2, &[]), row(3, &[4]), row(4, &[2])];
        assert_eq!(generations(merge), [4, 1, 3, 2]);
        // a commit that names its parent twice
        let twice = vec![row(1, &[2, 2]), row(2, &[3]), row(3, &[])];
        assert_eq!(generations(twice), [3, 2, 1]);
        let mut cycle = vec![row(1, &[2]), row(2, &[3]), row(3, &[1])];
        assert!(matches!(
            assign(&mut cycle, &none),
            Err(Error::Format("commit ancestry cycle"))
        ));
    }
}
