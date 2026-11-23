//! What whoever drives handles on a version 2 log knows of its ops
//! without a handle telling it: the ops it had appended, each with the
//! id its body hashes to and at the place the bytes of the file hold that
//! body, the ones it had cut, and the ones a power cut took, by the bytes
//! that were damaged. The log is held to it: it holds these ops, in this
//! order, at these places, and no other.

use std::collections::HashSet;
use std::fs::File;
use std::os::unix::fs::FileExt;
use std::path::Path;

use alt_oplog::{Anchor, Discarded, Meta, OpId, ROOT};

use super::v2_model::Reference;

#[derive(Debug, Clone)]
pub struct Entry {
    /// the number the op was appended under
    pub n: u64,
    /// how many ops the log held with it
    pub seq: u64,
    pub payload: Vec<u8>,
    pub id: OpId,
    pub at: u64,
    pub end: u64,
    pub linked: bool,
    pub request: Option<[u8; 16]>,
}

/// The number an op was appended under, which its payload begins with.
pub fn number(payload: &[u8]) -> u64 {
    let digits = payload[3..].iter().take_while(|byte| byte.is_ascii_digit());
    let digits: Vec<u8> = digits.copied().collect();
    let text = String::from_utf8(digits).unwrap();
    text.parse().expect("an op the driver appended")
}

/// Where in the log file of `dir`, at or after byte `after`, the op frame
/// lies that holds `body`, closes with `id` and counts itself op number
/// `seq`: found by those bytes, with the headers of the blocks of `block`
/// bytes taken out from between the bytes of frames.
fn lies(
    dir: &Path,
    block: u64,
    after: u64,
    (body, id, seq): (&[u8], OpId, u64),
) -> Option<(u64, u64)> {
    let file = File::open(dir.join("log")).unwrap();
    let len = file.metadata().unwrap().len().checked_sub(after)?;
    let mut read = vec![0u8; len as usize];
    file.read_exact_at(&mut read, after).unwrap();
    let places = (after..after + len).zip(read);
    let of_frames = |(place, _): &(u64, u8)| *place < block || place % block >= 32;
    let (places, bytes): (Vec<u64>, Vec<u8>) = places.filter(of_frames).unzip();
    let closed = [body, &id.0[..]].concat();
    let found = bytes
        .windows(closed.len())
        .position(|held| held == closed)?;
    // its length and its checksum follow the id
    let end = places.get(found + closed.len() + 7)? + 1;
    // 48 bytes of header and what the frame carries beside its body come
    // before the body; the header says how long that is, how long the
    // body, and which op of the log this is
    let word = |at: usize, len: usize| {
        let mut word = [0u8; 8];
        word[..len].copy_from_slice(&bytes[at..at + len]);
        u64::from_le_bytes(word)
    };
    let starts = |start: &usize| {
        bytes[*start] == 1
            && word(start + 2, 2) == (found - start - 48) as u64
            && word(start + 4, 4) == body.len() as u64
            && word(start + 40, 8) == seq
    };
    let start = (0..=found.checked_sub(48)?).rev().find(starts)?;
    Some((places[start], end))
}

#[derive(Default)]
pub struct Ledger {
    pub ops: Vec<Entry>,
    /// the payloads of the ops that were cut or lost
    gone: HashSet<Vec<u8>>,
    /// the ops that were cut or lost last
    pub was: Vec<Entry>,
    /// Where a handle may take the log to end before its last op: at the
    /// second place, in a file as long as the first says. A reader found
    /// an op there it does not vouch for, or a cut at it was begun and
    /// never came to the log. None holds beyond the next cut.
    pub hides: Vec<(u64, u64)>,
    /// where a cut was begun that never came to the log: a handle that
    /// holds frames past it is told they were cut
    pub begun: Option<u64>,
}

impl Ledger {
    /// Takes in an op of `payload` that `actor` had a handle append after
    /// the ops known here, to the log in `dir` with blocks of `block`
    /// bytes. Its id is the hash of the body it has after the last of
    /// them, it is the next op of the log, and it lies where the file
    /// holds that body. The handle that appended it has it at `told`.
    pub fn appended(
        &mut self,
        (dir, block): (&Path, u64),
        (actor, payload): (&str, &[u8]),
        meta: &Meta,
        told: Anchor,
    ) {
        let (parent, after) = self
            .ops
            .last()
            .map_or((ROOT, 64), |last| (last.id, last.end));
        let body = alt_oplog::v2::op_body(&parent, 1, actor, payload);
        let id = OpId(*alt_hash::blake3::hash(&body).as_bytes());
        let seq = self.ops.len() as u64 + 1;
        let n = number(payload);
        let Some((at, end)) = lies(dir, block, after, (&body, id, seq)) else {
            panic!("op {n} is not in the file after byte {after} as op {seq} of the log");
        };
        let found = Anchor {
            op: id,
            start: at,
            end,
            count: seq,
        };
        assert_eq!(
            told, found,
            "the handle that appended op {n} has it elsewhere than the file"
        );
        self.ops.push(Entry {
            n,
            seq,
            payload: payload.to_vec(),
            id,
            at,
            end,
            linked: meta.linked,
            request: meta.request,
        });
    }

    /// Where the op of `payload` starts, which the log holds.
    pub fn place_of(&self, payload: &[u8]) -> u64 {
        let op = self.ops.iter().find(|op| op.payload == payload);
        op.expect("the op is in the log").at
    }

    /// Whether the log holds the op at `head` where the anchor says.
    pub fn holds(&self, head: Option<Anchor>) -> bool {
        let Some(head) = head else {
            return true;
        };
        let op = (head.count as usize).checked_sub(1);
        let op = op.and_then(|index| self.ops.get(index));
        op.is_some_and(|op| (op.id, op.at, op.end) == (head.op, head.start, head.end))
    }

    fn went(&mut self, from: usize, synced: u64) -> Vec<Entry> {
        let gone = self.ops.split_off(from);
        for op in &gone {
            assert!(
                op.end > synced,
                "op {} is gone from within where the log is synced, {synced}",
                op.n
            );
        }
        self.gone.extend(gone.iter().map(|op| op.payload.clone()));
        if !gone.is_empty() {
            self.was = gone.clone();
        }
        gone
    }

    /// The op of `payload` and all after it are cut off the log, which is
    /// synced up to `synced`.
    pub fn cut_from(&mut self, payload: &[u8], synced: u64) -> Vec<Entry> {
        let from = self.ops.iter().position(|op| op.payload == payload);
        self.went(from.expect("the op to cut is in the log"), synced)
    }

    /// The bytes of the file from `byte` on were damaged: an op whose
    /// frame does not end before is lost, and every op after it.
    pub fn lost_from(&mut self, byte: u64, synced: u64) {
        let from = self.ops.iter().position(|op| op.end > byte);
        self.went(from.unwrap_or(self.ops.len()), synced);
    }

    /// Where the first op of `request` starts, and how many ops it has.
    pub fn of_request(&self, request: [u8; 16]) -> (u64, u32) {
        let of_it = |op: &&Entry| op.request == Some(request);
        let first = self.ops.iter().find(of_it).map_or(0, |op| op.at);
        (first, self.ops.iter().filter(of_it).count() as u32)
    }

    /// Holds the log as its file tells it to what was done to it: its ops
    /// are in the order they were appended in, none of them was cut or
    /// lost before, and they are the ops appended and neither cut nor
    /// lost, each where its handle put it.
    pub fn agrees(&self, log: &Reference) {
        let mut last = 0;
        for op in &log.ops {
            let n = number(&op.payload);
            assert!(n > last, "op {n} follows op {last} in the log");
            assert!(
                !self.gone.contains(&op.payload),
                "op {n} at {} is in the log again: it was cut or lost",
                op.at
            );
            last = n;
        }
        let place = |n: u64, id: OpId, at: u64, end: u64| (n, id, at, end);
        let laid = log.ops.iter();
        let laid: Vec<_> = laid
            .map(|op| place(number(&op.payload), op.id, op.at, op.end))
            .collect();
        let known: Vec<_> = self
            .ops
            .iter()
            .map(|op| place(op.n, op.id, op.at, op.end))
            .collect();
        assert_eq!(
            laid, known,
            "the log holds other ops than its writers left in it"
        );
    }

    /// Holds what a handle tells was cut off the log to what was done:
    /// `cut` are the ops its look had cut, which the last of `told` names
    /// one by one; whatever else it tells of followed something that did
    /// not read, and its ops were lost before.
    pub fn reconciled(&self, told: &[Discarded], cut: Option<&[Entry]>) {
        let mut rest = told;
        if let Some(cut) = cut {
            let (last, before) = told.split_last().expect("a cut is told");
            let named: Vec<&[u8]> = last.ops.iter().map(|op| &op.payload[..]).collect();
            let gone: Vec<&[u8]> = cut.iter().map(|op| &op.payload[..]).collect();
            assert_eq!(named, gone, "what is told as cut is not what was cut");
            assert_eq!((last.from, last.unread), (cut[0].at, 0));
            assert!(last.to >= cut.last().unwrap().end);
            rest = before;
        }
        for op in rest.iter().flat_map(|told| &told.ops) {
            assert!(
                self.gone.contains(&op.payload),
                "op {} is told as lying past the log: nothing took it",
                number(&op.payload)
            );
        }
    }
}
