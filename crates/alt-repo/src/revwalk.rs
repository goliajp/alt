use std::cmp::Reverse;
use std::collections::BinaryHeap;

use alt_git_codec::{Commit, ObjectId, ObjectKind};
use alt_hash::IdSet;

use crate::{RepoError, Repository};

/// History traversal in git's default `log` order: commits by descending
/// committer date, FIFO among equal dates (matching git's date-ordered
/// commit list insertion).
///
/// Each commit is read from the store once, when it enters the queue, and
/// a commit's parents enter the queue only when the walk is asked for the
/// commit after it: a walk cut short reads nothing it does not show, apart
/// from the parents it had to order.
pub struct RevWalk<'r> {
    repo: &'r Repository,
    queue: BinaryHeap<Queued>,
    seen: IdSet<ObjectId>,
    seq: u64,
    /// parents of the commit handed out last, not queued yet
    pending: Vec<ObjectId>,
}

struct Queued {
    date: i64,
    order: Reverse<u64>,
    oid: ObjectId,
    commit: Commit,
    data: Vec<u8>,
}

impl PartialEq for Queued {
    fn eq(&self, other: &Self) -> bool {
        self.order == other.order
    }
}

impl Eq for Queued {}

impl Ord for Queued {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // max-heap: newest date pops first; among equal dates the smallest
        // sequence number (= earliest queued) pops first via `Reverse`.
        // Sequence numbers are unique, so they settle every tie
        self.date
            .cmp(&other.date)
            .then(self.order.cmp(&other.order))
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
            seen: IdSet::default(),
            seq: 0,
            pending: Vec::new(),
        };
        walk.push(start)?;
        Ok(walk)
    }

    fn push(&mut self, oid: ObjectId) -> Result<(), RepoError> {
        if !self.seen.insert(oid) {
            return Ok(());
        }
        let obj = self
            .repo
            .read_object(&oid)?
            .ok_or(RepoError::MissingObject(oid))?;
        if obj.kind != ObjectKind::Commit {
            return Err(RepoError::Format("expected a commit object"));
        }
        let commit = Commit::parse(&obj.data)?;
        let date = commit.committer_date().unwrap_or(0);
        self.seq += 1;
        self.queue.push(Queued {
            date,
            order: Reverse(self.seq),
            oid,
            commit,
            data: obj.data,
        });
        Ok(())
    }

    /// The next commit with the bytes of its object, for a caller that
    /// prints them and would otherwise read the object again.
    pub fn next_with_data(&mut self) -> Option<Result<(ObjectId, Commit, Vec<u8>), RepoError>> {
        for parent in std::mem::take(&mut self.pending) {
            if let Err(e) = self.push(parent) {
                return Some(Err(e));
            }
        }
        let next = self.queue.pop()?;
        self.pending.extend(next.commit.parents());
        Some(Ok((next.oid, next.commit, next.data)))
    }
}

impl Iterator for RevWalk<'_> {
    type Item = Result<(ObjectId, Commit), RepoError>;

    fn next(&mut self) -> Option<Self::Item> {
        let next = self.next_with_data()?;
        Some(next.map(|(oid, commit, _)| (oid, commit)))
    }
}
