//! What whoever drives handles on a version 2 log knows of its ops
//! without reading it: the ops it appended and where each handle said it
//! put them, the ones it had cut, and the ones a power cut took, by the
//! bytes that were damaged. The log is held to it: it holds these ops,
//! in this order, at these places, and no other.

use std::collections::HashSet;

use alt_oplog::{Anchor, Discarded, Meta, OpId};

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
    /// Takes in an op a handle appended and holds as its last, at `head`.
    pub fn appended(&mut self, payload: &[u8], meta: &Meta, head: Anchor) {
        let after = self.ops.last().map_or(0, |last| last.end);
        assert!(head.start >= after, "an op was appended over another");
        assert_eq!(
            head.count as usize,
            self.ops.len() + 1,
            "the op appended is not counted as the next of the log"
        );
        self.ops.push(Entry {
            n: number(payload),
            seq: head.count,
            payload: payload.to_vec(),
            id: head.op,
            at: head.start,
            end: head.end,
            linked: meta.linked,
            request: meta.request,
        });
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
