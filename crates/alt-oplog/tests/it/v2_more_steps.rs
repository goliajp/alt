//! More of what a driven sequence does to a version 2 log: the receipt
//! of a request, an open at the anchor of an op, and a log that is cut
//! and grows back to the very length it had.

use std::sync::atomic::Ordering;

use alt_oplog::v2::frame::frame_len;
use alt_oplog::v2::receipt::{Outcome, Receipt};
use alt_oplog::{Anchor, Meta, OpLog, OpLogError, ROOT};

use super::v2_driver::{Run, name};
use super::v2_owed::Wrote;

impl Run {
    /// Appends `payload` through handle `index` as the op `meta` says,
    /// synced or not. Whoever syncs what ops name fails with `fails`.
    /// Whether it was appended.
    pub fn put(&mut self, index: usize, op: (Meta, Vec<u8>), unsynced: bool, fails: bool) -> bool {
        let (meta, payload) = op;
        let unsettled = self.unsettled_before(u64::MAX);
        self.trace.push(format!(
            "handle {index} appends {} (workspace {:?}, linked: {}, staged: {}, retired: {}), \
             unsynced: {unsynced}",
            name(&payload),
            meta.workspace.map(|workspace| workspace[0]),
            meta.linked,
            meta.index_after.is_some(),
            meta.retired
        ));
        self.calls();
        self.refuses.store(fails, Ordering::Relaxed);
        let left = unsynced.then(|| alt_os::durability::leave_unsynced(true));
        let appended = self.handles[index]
            .log
            .append_with("driver", 1, &payload, &meta);
        drop(left);
        self.refuses.store(false, Ordering::Relaxed);
        if !matches!(appended, Err(OpLogError::CutBack)) {
            // it read the log to its end under the lock, whatever came
            // of the append after
            self.ledger.begun = None;
        }
        let seen_to = self.calls() != 0;
        self.meanwhile_done(&unsettled);
        if self.took(index, appended, fails).is_none() {
            return false;
        }
        let head = self.handles[index].log.anchor().unwrap();
        self.handles[index].mine.push(Wrote::Op(head));
        self.appended("driver", payload, (&meta, unsynced), head);
        // a writer that syncs appends after nothing that is not seen to
        if let Some(op) = unsettled.first().filter(|_| !unsynced && !seen_to) {
            panic!(
                "appended after {} at {}, which another boot left and nobody vouched for",
                name(&op.payload),
                op.at
            );
        }
        true
    }

    /// A writer appends the receipt of the request the last ops were of:
    /// it says of them what the driver knows is left of them in the log.
    pub fn receipt(&mut self) {
        let index = self.writer();
        let request = [self.request; 16];
        let (first_op, ops) = self.ledger.of_request(request);
        let mut receipt = Receipt {
            request,
            binding: [7; 32],
            outcome: Outcome::Executed,
            exit: 0,
            first_op,
            ops,
            finished_ms: 1,
            digest: [0; 32],
            len: 0,
            response: None,
        };
        receipt.answer(b"done");
        let unsynced = self.rng.one_in(2);
        self.trace.push(format!(
            "handle {index} appends the receipt of request {}: {ops} ops from {first_op}",
            self.request
        ));
        let unsettled = self.unsettled_before(u64::MAX);
        let left = unsynced.then(|| alt_os::durability::leave_unsynced(true));
        let appended = self.handles[index].log.append_receipt(&receipt);
        drop(left);
        self.meanwhile_done(&unsettled);
        if self.took(index, appended, false).is_some() {
            let after = self.ledger.ops.len();
            self.ledger.receipts.push((after, !unsynced, request));
            let handle = &mut self.handles[index];
            let end = handle.log.appended_len();
            handle.mine.push(Wrote::Receipt(request, end));
            self.request += 1;
            self.ledger.begun = None;
            self.counts.receipts.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Opens a handle at the anchor of an op of the log, or of one that
    /// was cut or lost: it begins after the op while the log holds it.
    pub fn open_anchored(&mut self) {
        let stale = !self.ledger.was.is_empty() && self.rng.one_in(4);
        let ops = match stale {
            true => &self.ledger.was,
            false => &self.ledger.ops,
        };
        if ops.is_empty() {
            return;
        }
        let op = ops[self.rng.below(ops.len() as u64) as usize].clone();
        let anchor = Anchor {
            op: op.id,
            start: op.at,
            end: op.end,
            count: op.seq,
        };
        self.trace.push(format!(
            "open at the anchor of {} (gone: {stale})",
            name(&op.payload)
        ));
        let opened = OpLog::open_at(self.path(), Some(anchor)).expect("the log opens");
        let index = self.adopt(opened, false);
        let log = &self.handles[index].log;
        let (held, end) = (log.len(), log.appended_len());
        assert!(
            self.holds_enough(held, end),
            "a handle opened at an anchor holds {held} ops up to {end}"
        );
        if stale {
            assert!(!log.anchored(), "a handle begins after an op that is gone");
            self.counts.anchored_gone.fetch_add(1, Ordering::Relaxed);
        } else if held == self.ledger.ops.len() {
            assert!(log.anchored(), "the anchor of an op of the log is not held");
            // of the ops after it, in their order: every ref transaction,
            // and the last op of the log
            let mut after = self.ledger.ops[op.seq as usize..].iter();
            let recent = log.recent();
            for held in recent {
                assert!(after.any(|op| op.id == held.id), "an op out of its place");
            }
            let linked = self.ledger.ops[op.seq as usize..].iter();
            let linked: Vec<_> = linked.filter(|op| op.linked).map(|op| op.id).collect();
            let held = recent.iter().filter(|op| op.frame.unwrap().linked);
            assert_eq!(held.map(|op| op.id).collect::<Vec<_>>(), linked);
            self.counts.anchored.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// The log is cut, of what was never finished or of ops nobody
    /// vouches for, the cut dying or not once the file is shorter, and
    /// then grows back to the very length it had before. Readers that
    /// were told where the log ends read on after.
    pub fn grow_back(&mut self) {
        let dies = match self.rng.below(3) {
            0 => None,
            step => Some(3 + step as u8),
        };
        let died = self.died;
        let before = match self.rng.below(3) {
            0 => {
                // more than the checkpoint of the cut and an op take
                let len = 600 + self.rng.below(900);
                self.tear_of(len);
                let before = self.file_len();
                let index = self.writer();
                self.read_on(index, true, dies);
                before
            }
            way => {
                // what is not synced is another boot's to look at
                self.reboot(true);
                let before = self.file_len();
                if way == 2 {
                    // a reader notes where the log ends in this file, and
                    // another is opened that is told so
                    self.open(true);
                    self.look_and_refuse();
                    self.open(true);
                }
                let index = self.writer();
                self.settle(index, dies, true);
                before
            }
        };
        self.fill(before, self.died != died);
        for index in 0..self.handles.len() {
            if self.handles[index].read_only && self.ledger.holds(self.handles[index].log.anchor())
            {
                self.read_on(index, false, None);
            }
        }
    }

    /// Appends an op that makes the file `len` bytes long again, where
    /// the block the log ends in has the room.
    fn fill(&mut self, len: u64, after_death: bool) {
        let index = self.writer();
        let empty = alt_oplog::v2::op_body(&ROOT, 1, "driver", b"").len();
        let taken = self.file_len() + frame_len(0, empty, true) + self.next_name();
        let Some(padding) = len.checked_sub(taken) else {
            return;
        };
        let payload = self.payload(padding);
        let unsynced = self.rng.one_in(2);
        if !self.put(index, (Meta::default(), payload), unsynced, false) {
            return;
        }
        if self.file_len() == len {
            self.trace
                .push(format!("  the file is {len} bytes long again"));
            self.counts.grown_back.fetch_add(1, Ordering::Relaxed);
            if after_death {
                let grown = &self.counts.grown_back_after_death;
                grown.fetch_add(1, Ordering::Relaxed);
            }
        }
        if !unsynced && self.rng.one_in(2) {
            self.sync(index, 0);
        }
    }
}
