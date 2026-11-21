//! How far two commits are apart: the commits each has that the other
//! does not.

use std::collections::{BinaryHeap, HashMap, HashSet};
use std::hash::Hash;

use super::*;

const OURS: u8 = 1;
const THEIRS: u8 = 2;
const BOTH: u8 = OURS | THEIRS;

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Distance {
    /// commits only ours has
    pub ahead: usize,
    /// commits only theirs has
    pub behind: usize,
    /// the walk was cut short: both counts are at least what they say
    pub cut: bool,
}

/// The commits looked at so far, each with the sides it is reachable
/// from.
struct Walk<C, R> {
    /// the date and the parents of a commit
    read: R,
    sides: HashMap<C, u8>,
    known: HashMap<C, (i64, Vec<C>)>,
    /// newest first
    queue: BinaryHeap<(i64, C)>,
    /// queued commits not yet known to be on both sides
    one_sided: HashSet<C>,
    /// the oldest date a commit of one side was seen with
    oldest_one_sided: i64,
}

impl<C: Copy + Ord + Hash, R: FnMut(C) -> Res<(i64, Vec<C>)>> Walk<C, R> {
    /// Marks `commit` reachable from `sides` and queues it when that is
    /// news.
    fn reach(&mut self, commit: C, sides: u8) -> Res<()> {
        let before = self.sides.get(&commit).copied().unwrap_or(0);
        if before | sides == before {
            return Ok(());
        }
        if !self.known.contains_key(&commit) {
            let read = (self.read)(commit)?;
            self.known.insert(commit, read);
        }
        let date = self.known[&commit].0;
        self.sides.insert(commit, before | sides);
        self.queue.push((date, commit));
        if before | sides == BOTH {
            self.one_sided.remove(&commit);
        } else {
            self.one_sided.insert(commit);
            self.oldest_one_sided = self.oldest_one_sided.min(date);
        }
        Ok(())
    }
}

/// How many commits `ours` has that `theirs` does not, and the other way
/// round, looking at no more than `most` commits; `read` gives the date
/// and the parents of a commit.
///
/// Commits are taken newest first and hand the sides they are reachable
/// from to their parents. Once every commit waiting is on both sides,
/// what lies beyond is common history, unless a commit waiting is no
/// older than a commit counted for one side: it may still lead to that
/// commit, so the walk goes on until none is. The counts are exact
/// wherever no commit is dated before its parent.
pub(super) fn distance<C: Copy + Ord + Hash>(
    ours: C,
    theirs: C,
    most: usize,
    read: impl FnMut(C) -> Res<(i64, Vec<C>)>,
) -> Res<Distance> {
    if ours == theirs {
        return Ok(Distance {
            ahead: 0,
            behind: 0,
            cut: false,
        });
    }
    let mut walk = Walk {
        read,
        sides: HashMap::new(),
        known: HashMap::new(),
        queue: BinaryHeap::new(),
        one_sided: HashSet::new(),
        oldest_one_sided: i64::MAX,
    };
    walk.reach(ours, OURS)?;
    walk.reach(theirs, THEIRS)?;
    let mut cut = false;
    while let Some((date, commit)) = walk.queue.pop() {
        if walk.one_sided.is_empty() && date < walk.oldest_one_sided {
            break;
        }
        if walk.sides.len() > most {
            cut = true;
            break;
        }
        walk.one_sided.remove(&commit);
        let sides = walk.sides[&commit];
        for parent in walk.known[&commit].1.clone() {
            walk.reach(parent, sides)?;
        }
    }
    let count = |side: u8| walk.sides.values().filter(|sides| **sides == side).count();
    Ok(Distance {
        ahead: count(OURS),
        behind: count(THEIRS),
        cut,
    })
}

impl NativeRepo<'_> {
    /// [`distance`] between two commits of this repository.
    pub(super) fn ahead_behind(
        &self,
        ours: ObjectId,
        theirs: ObjectId,
        most: usize,
    ) -> Res<Distance> {
        distance(ours, theirs, most, |commit| {
            let object = self.store.odb.get(&commit)?;
            let object = object.ok_or("commit missing from store")?;
            let parsed = alt_git_codec::Commit::parse(&object.data)?;
            let date = parsed.committer_date().unwrap_or(0);
            Ok((date, parsed.parents().collect()))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The distance in a history given as `(commit, date, parents)`, and
    /// how many commits were read for it.
    fn apart(
        history: &[(u32, i64, &[u32])],
        ours: u32,
        theirs: u32,
        most: usize,
    ) -> (Distance, usize) {
        let mut read = 0;
        let found = distance(ours, theirs, most, |commit| {
            read += 1;
            let known = history.iter().find(|(id, _, _)| *id == commit).unwrap();
            Ok((known.1, known.2.to_vec()))
        });
        (found.unwrap(), read)
    }

    fn counts(ahead: usize, behind: usize) -> Distance {
        Distance {
            ahead,
            behind,
            cut: false,
        }
    }

    /// 1 ← 2 ← 3 ← 4 ← 5 on one line, and 6 ← 7 branching off 3
    const FORKED: &[(u32, i64, &[u32])] = &[
        (1, 10, &[]),
        (2, 20, &[1]),
        (3, 30, &[2]),
        (4, 40, &[3]),
        (5, 50, &[4]),
        (6, 45, &[3]),
        (7, 55, &[6]),
    ];

    #[test]
    fn the_same_commit_is_no_distance_and_reads_nothing() {
        assert_eq!(apart(FORKED, 5, 5, 100), (counts(0, 0), 0));
    }

    #[test]
    fn one_side_ahead_counts_its_commits_and_stops_at_the_other() {
        let (found, read) = apart(FORKED, 5, 3, 100);
        assert_eq!(found, counts(2, 0));
        // 5, 4, 3 and the parent of 3, which shows the rest is common
        assert_eq!(read, 4);
        assert_eq!(apart(FORKED, 3, 5, 100).0, counts(0, 2));
    }

    #[test]
    fn both_sides_count_what_lies_above_where_they_forked() {
        assert_eq!(apart(FORKED, 5, 7, 100).0, counts(2, 2));
    }

    #[test]
    fn a_merge_of_the_other_side_leaves_only_the_commits_it_lacks() {
        // 8 merges 5 and 7
        let mut history = FORKED.to_vec();
        history.push((8, 60, &[5, 7]));
        assert_eq!(apart(&history, 8, 7, 100).0, counts(3, 0));
        assert_eq!(apart(&history, 8, 5, 100).0, counts(3, 0));
        assert_eq!(apart(&history, 5, 8, 100).0, counts(0, 3));
    }

    #[test]
    fn a_commit_dated_after_its_child_is_still_found_common() {
        // ours 4 ← 3 ← 2 ← 1, theirs 5 ← 2; 2 carries the newest date of
        // all, so it is read as theirs before 3 hands it ours
        let history: &[(u32, i64, &[u32])] = &[
            (1, 10, &[]),
            (2, 90, &[1]),
            (3, 30, &[2]),
            (4, 40, &[3]),
            (5, 95, &[2]),
        ];
        assert_eq!(apart(history, 4, 5, 100).0, counts(2, 1));
    }

    #[test]
    fn a_commit_found_common_late_hands_that_on_to_its_parents() {
        // ours 1 ← 2 ← 4, theirs 2 ← 3 ← 5; 3 is dated before 2 and 1,
        // so both are counted for ours before 3 is read
        let history: &[(u32, i64, &[u32])] = &[
            (1, 40, &[]),
            (2, 50, &[1]),
            (3, 10, &[2]),
            (4, 60, &[2]),
            (5, 100, &[3]),
        ];
        assert_eq!(apart(history, 4, 5, 100).0, counts(1, 2));
    }

    #[test]
    fn a_walk_past_the_most_commits_is_cut_and_says_so() {
        let found = apart(FORKED, 5, 7, 3).0;
        assert!(found.cut);
        assert!(found.ahead <= 2 && found.behind <= 2, "{found:?}");
    }
}
