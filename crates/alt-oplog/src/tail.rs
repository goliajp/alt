//! The end of the log as a writer finds it under the log's lock: the
//! records other writers appended, and what it cuts off after them.
//!
//! A record that does not read past the synced boundary ends the log for
//! every reader, and a writer cuts the log there. What it cuts may hold
//! whole records after that one, written out by the kernel while the
//! record before them was not: they were never synced either, and go with
//! it. Each cut is kept ([`Discarded`]) for whoever tells the user.

use crate::lock::{lock_shared, unlock};
use crate::read::{read_at, unfinished};
use crate::scan::{BODY_FIXED_LEN, CHECKSUM_LEN, ends_the_log, parse_body, replay_records};
use crate::synced::{self, Marker};
use crate::{Op, OpId, OpLog, OpLogError, ROOT};

/// Bytes a writer cut off the end of the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discarded {
    /// where the log ends now
    pub from: u64,
    /// where it ended
    pub to: u64,
    /// the ops among those bytes whose records read, in the order they
    /// were in
    pub ops: Vec<Op>,
    /// how many of those bytes no record that reads covers: none when
    /// whole ops were cut for what they name
    pub unread: u64,
}

impl Discarded {
    /// The bytes `rest`, which ended a log `to` bytes long and begin with a
    /// record that does not read, cut off it at `from`.
    fn unread(from: u64, to: u64, rest: &[u8]) -> Self {
        let (ops, unread) = whole_ops(rest);
        Self {
            from,
            to,
            ops,
            unread,
        }
    }
}

/// The op whose record begins at `at` of `rest` and reads, with the length
/// of the record.
fn whole_at(rest: &[u8], at: usize) -> Option<(Op, usize)> {
    let body_len = u32::from_le_bytes(rest.get(at..at + 4)?.try_into().unwrap()) as usize;
    if body_len < BODY_FIXED_LEN {
        return None;
    }
    let record = rest.get(at + 4..(at + 4 + body_len).checked_add(CHECKSUM_LEN)?)?;
    let (body, check) = record.split_at(body_len);
    let hash = alt_hash::blake3::hash(body);
    if hash.as_bytes()[..CHECKSUM_LEN] != *check {
        return None;
    }
    let op = parse_body(body, OpId(*hash.as_bytes())).ok()?;
    Some((op, 4 + body_len + CHECKSUM_LEN))
}

/// The ops whose records read in `rest`, wherever they begin, and how many
/// bytes of it none of them covers: every such byte is tried as the start
/// of a record.
pub(crate) fn whole_ops(rest: &[u8]) -> (Vec<Op>, u64) {
    let (mut at, mut ops, mut unread) = (0, Vec::new(), 0);
    while at < rest.len() {
        match whole_at(rest, at) {
            Some((op, len)) => {
                ops.push(op);
                at += len;
            }
            None => {
                unread += 1;
                at += 1;
            }
        }
    }
    (ops, unread)
}

impl OpLog {
    /// Replays any records appended past our known tail (by another writer),
    /// extending the in-memory chain so the next append parents on the real
    /// head. A torn tail left by a crashed writer is truncated (we hold the
    /// lock, so no live writer is mid-append).
    pub(crate) fn catch_up(&mut self) -> Result<(), OpLogError> {
        self.read_on(true)
    }

    /// [`Self::refresh`] for a handle that serves a read: the records other
    /// writers appended are replayed under the shared lock, and nothing is
    /// written. What a crashed writer left unfinished is passed over, as an
    /// open passes over it; the next append cuts it off.
    pub fn follow(&mut self) -> Result<(), OpLogError> {
        lock_shared(&self.file)?;
        let read = self.read_on(false);
        let _ = unlock(&self.file);
        read
    }

    /// Replays the records past the ones held, under the log's lock. An
    /// unfinished end is cut off the file when `cut`, which takes the
    /// exclusive lock, and stays in it otherwise.
    fn read_on(&mut self, cut: bool) -> Result<(), OpLogError> {
        let size = self.file.metadata()?.len();
        if !self.holds_head(size)? {
            return Err(OpLogError::CutBack);
        }
        if size <= self.len_bytes {
            return Ok(());
        }
        let tail = read_at(&self.file, self.len_bytes, (size - self.len_bytes) as usize)?;

        let head = self.head().unwrap_or(ROOT);
        let known = self.ops.len();
        let replayed = replay_records(&tail, 0, head, &mut self.ops);
        let replayed = replayed.and_then(|replayed| {
            let new_len = self.len_bytes + replayed.valid as u64;
            if replayed.valid < tail.len() {
                let last = ends_the_log(&tail, replayed.valid);
                unfinished(&self.durable_path, new_len, last)?;
            }
            Ok(replayed)
        });
        let replayed = match replayed {
            Ok(replayed) => replayed,
            Err(damage) => {
                // the handle stays where it was
                self.ops.truncate(known);
                return Err(damage);
            }
        };
        if let Some(full) = self.full.get_mut() {
            full.extend_from_slice(&self.ops[known..]);
        }
        let new_len = self.len_bytes + replayed.valid as u64;
        if let Some(start) = replayed.last_start {
            self.last_start = self.len_bytes + start as u64;
            self.others_to = new_len;
        }
        if cut && replayed.valid < tail.len() {
            self.cut_unread(new_len, size, &tail[replayed.valid..])?;
        }
        self.passed = !cut && replayed.valid < tail.len();
        self.len_bytes = new_len;
        Ok(())
    }

    /// Drops a crashed writer's unfinished record at `at` of a log `size`
    /// bytes long, and what follows it, `rest`. A handle that appended a
    /// whole record there records nothing as synced under another
    /// generation.
    fn cut_unread(&mut self, at: u64, size: u64, rest: &[u8]) -> Result<(), OpLogError> {
        let discarded = Discarded::unread(at, size, rest);
        match discarded.ops.is_empty() {
            true => synced::cut(&self.durable_path, &self.file, at)?,
            false => synced::cut_records(&self.durable_path, &self.file, at)?,
        }
        self.note_discarded(discarded);
        Ok(())
    }

    /// Whether the last read of this handle passed over an unfinished
    /// append at the end of the log: whole ops may follow it there
    /// ([`Self::passed_ops`]).
    pub fn passed_over(&self) -> bool {
        self.passed
    }

    /// For the full check: the ops past the ones this handle holds whose
    /// records read, wherever they begin (see [`Self::passed`]).
    pub fn passed_ops(&self) -> Result<Vec<Op>, OpLogError> {
        let len = self.file.metadata()?.len();
        let rest = len.saturating_sub(self.len_bytes) as usize;
        Ok(whole_ops(&read_at(&self.file, self.len_bytes, rest)?).0)
    }

    /// For a check: from which byte on the records of the log are not
    /// known to have been synced, where the marker records a boundary
    /// that holds, or one that appends were left unsynced past in
    /// another boot ([`crate::left`]). Reads the marker and takes no lock.
    pub fn unsynced_from(&self) -> Option<u64> {
        let marker = Marker::read(&self.durable_path);
        marker.unsynced_from().or_else(|| self.left_in_boot(false))
    }

    /// The boundary appends were left unsynced past, where they were
    /// left so in the running boot (`running`) or in another.
    fn left_in_boot(&self, running: bool) -> Option<u64> {
        let marker = Marker::read(&self.durable_path);
        let (from, left) = marker.judged_from()?;
        (left && crate::left::cached(&self.durable_path) == running).then_some(from)
    }

    /// For a check: the boundary past which appends were left unsynced
    /// in the running boot. A record that does not read past it was
    /// never confirmed either, and the check tells it apart from one
    /// before the boundary.
    pub fn left_unsynced_from(&self) -> Option<u64> {
        self.left_in_boot(true)
    }

    /// What this handle cut off the end of the log since the last call,
    /// in the order it was cut.
    pub fn take_discarded(&mut self) -> Vec<Discarded> {
        std::mem::take(&mut self.discarded)
    }

    pub(crate) fn note_discarded(&mut self, discarded: Discarded) {
        self.discarded.push(discarded);
    }
}
