use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashSet};

use alt_git_codec::{Commit, ObjectId};

use crate::{RepoError, Repository};

/// History traversal in git's default `log` order: commits by descending
/// committer date, FIFO among equal dates (matching git's date-ordered
/// commit list insertion).
pub struct RevWalk<'r> {
    repo: &'r Repository,
    queue: BinaryHeap<Queued>,
    seen: HashSet<ObjectId>,
    seq: u64,
}

#[derive(PartialEq, Eq)]
struct Queued {
    date: i64,
    order: Reverse<u64>,
    oid: ObjectId,
}

impl Ord for Queued {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // max-heap: newest date pops first; among equal dates the smallest
        // sequence number (= earliest queued) pops first via `Reverse`
        self.date
            .cmp(&other.date)
            .then(self.order.cmp(&other.order))
            .then_with(|| self.oid.as_bytes().cmp(other.oid.as_bytes()))
    }
}

impl PartialOrd for Queued {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<'r> RevWalk<'r> {
    pub(crate) fn new(repo: &'r Repository, start: ObjectId) -> Result<Self, RepoError> {
        let mut walk = Self {
            repo,
            queue: BinaryHeap::new(),
            seen: HashSet::new(),
            seq: 0,
        };
        walk.push(start)?;
        Ok(walk)
    }

    fn push(&mut self, oid: ObjectId) -> Result<(), RepoError> {
        if !self.seen.insert(oid) {
            return Ok(());
        }
        let commit = self.repo.read_commit(&oid)?;
        let date = commit.committer_date().unwrap_or(0);
        self.seq += 1;
        self.queue.push(Queued {
            date,
            order: Reverse(self.seq),
            oid,
        });
        Ok(())
    }
}

impl Iterator for RevWalk<'_> {
    type Item = Result<(ObjectId, Commit), RepoError>;

    fn next(&mut self) -> Option<Self::Item> {
        let next = self.queue.pop()?;
        let commit = match self.repo.read_commit(&next.oid) {
            Ok(commit) => commit,
            Err(e) => return Some(Err(e)),
        };
        for parent in commit.parents() {
            if let Err(e) = self.push(parent) {
                return Some(Err(e));
            }
        }
        Some(Ok((next.oid, commit)))
    }
}
