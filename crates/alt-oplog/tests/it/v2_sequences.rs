//! Sequences of what handles do to a version 2 log, drawn from a seed:
//! appends that are synced and that are not, with a look before them and
//! without, syncs, looks that vouch and that cut, receipts, opens at the
//! anchor of an op, appends that were never finished, cuts that die at
//! each of their steps, a log grown back to the length it had before a
//! cut, another handle that appends or cuts while a writer has given up
//! the lock, readers that open and read on at any time, and the machine
//! going down with a part of what was not synced lost. After every step
//! the log is held to what the driver did to it, and each handle to what
//! the file and the driver know: it takes for looked at, and for synced,
//! no more than they do.

use std::sync::atomic::Ordering;

use alt_oplog::OpLogError;

use super::v2_driver::Run;
use super::v2_meanwhile::{Plan, refused_of, says};
use super::v2_model::Laid;

impl Run {
    /// The ops before byte `end` that another boot left and nobody has
    /// vouched for, with the log synced as far as it is noted to be now.
    pub fn unsettled_before(&mut self, end: u64) -> Vec<Laid> {
        self.calls();
        let log = self.log();
        self.cuts_seen = self.cuts_known(&log);
        let claimed = self.claimed(&log);
        self.model.synced_to(&log, claimed);
        let left = self.model.unsettled(&log);
        left.into_iter().filter(|op| op.at < end).cloned().collect()
    }

    /// Has another handle append or look, with some chance, the next
    /// time a writer gives up the lock for whoever syncs what ops name.
    pub fn plan_meanwhile(&mut self) {
        let plan = match self.rng.below(8) {
            0 => {
                let (meta, payload) = self.op();
                let unsynced = self.rng.one_in(2);
                Some(Plan::Append {
                    meta,
                    payload,
                    unsynced,
                })
            }
            1 => {
                let _ = self.rng.one_in(2) && self.lose_one().is_some();
                Some(Plan::Look)
            }
            _ => None,
        };
        self.meanwhile.plan(plan);
    }

    fn append(&mut self) {
        let index = self.writer();
        if self.rng.one_in(3) && !self.settle(index, None, false) {
            return;
        }
        // what is done meanwhile comes into the log first
        self.plan_meanwhile();
        let op = self.op();
        let unsynced = self.rng.one_in(2);
        let fails = !unsynced && self.rng.one_in(8);
        self.to_look = false;
        let claims = self.handles[index].claims.len();
        if !self.put(index, op.clone(), unsynced, fails) {
            // refused for an op nobody looked at, it looks and appends
            let looks = std::mem::take(&mut self.to_look) && self.settle(index, None, false);
            if !(looks && self.put(index, op, unsynced, false)) {
                return;
            }
        }
        if !unsynced && self.rng.one_in(2) {
            // the caller of the op asks: with its ticket where it has one
            let ticketed = self.handles[index].claims.len() > claims;
            self.sync(index, if ticketed { 2 } else { 0 });
        }
    }

    /// Has the handle sync what it holds for a caller without a ticket
    /// (`how` 0), set a boundary over all the log holds as a writer
    /// does (1), or sync for a caller that had it write on a ticket (2,
    /// 3).
    pub fn sync(&mut self, index: usize, how: u64) {
        let unsettled = self.unsettled_before(u64::MAX);
        let fails = self.rng.one_in(8);
        self.trace
            .push(format!("handle {index} syncs (way {how}), fails: {fails}"));
        self.plan_meanwhile();
        self.refuses.store(fails, Ordering::Relaxed);
        let claims = self.handles[index].claims.len() as u64;
        let synced = match how {
            0 | 1 => self.sync_through(index, how, &unsettled),
            _ if claims != 0 => {
                // the ticket written on last, or any
                let claim = match how {
                    2 => claims - 1,
                    _ => self.rng.below(claims),
                };
                self.ask(index, claim as usize, &unsettled);
                self.asks_again(index);
                Ok(())
            }
            _ => Ok(()),
        };
        self.refuses.store(false, Ordering::Relaxed);
        // what was planned and found nobody that gave up the lock is off
        self.meanwhile_done(&unsettled);
        let by_sink = std::mem::take(&mut self.by_sink);
        match synced {
            Ok(()) => {}
            Err(OpLogError::CutBack) => self.trace.push(format!(
                "  its caller is told of a cut (by the sink: {by_sink})"
            )),
            Err(OpLogError::Io(_)) if fails || self.lacked_now() => {
                self.trace.push("  it fails as it was made to".into())
            }
            Err(OpLogError::Unjudged { at }) => self.unjudged(index, at),
            Err(other) => panic!("{other:?}"),
        }
        let told = self.handles[index].log.take_discarded();
        self.cuts_told(&told, None);
    }

    /// Any handle looks at what was left, as a reader does.
    fn look(&mut self, surely: bool) {
        let index = self.rng.below(self.handles.len() as u64) as usize;
        if !self.ledger.holds(self.handles[index].log.anchor()) {
            return;
        }
        let end = self.handles[index].log.appended_len();
        let unsettled = self.unsettled_before(end);
        if surely || self.rng.one_in(3) {
            self.lose_one();
        }
        self.trace.push(format!("handle {index} looks"));
        let (stored, mut asked) = (self.model.stored.clone(), Vec::new());
        let found = self.handles[index]
            .log
            .unconfirmed(says(&stored, &mut asked));
        // a reader is refused as a writer is, short of the end of the log
        if let Err(why @ OpLogError::Kept { .. }) = &found {
            // what it holds of the log may be less than a writer would
            let begun = self.ledger.begun;
            self.looks_behind = end != self.log().end;
            self.cut_refused(&refused_of(&asked), &asked, why, &[]);
            (self.ledger.begun, self.looks_behind) = (begun, false);
            return;
        }
        if let Some(found) = self.took(index, found, false) {
            // it ends at the op it was told is not held, and nowhere else
            let unheld = refused_of(&asked);
            let ends = unheld.map(|payload| self.ledger.place_of(&payload));
            assert_eq!(
                found.map(|found| found.at),
                ends,
                "a reader's look ends elsewhere than at the op it was told is not held"
            );
            self.looked(&unsettled, &asked, ends);
            // it may note that the log ends there in the file it read
            let seen = self.handles[index].seen.iter();
            let hides = seen.filter_map(|len| Some((*len, ends?)));
            self.ledger.hides.extend(hides);
        }
    }

    /// Handle `index` reads what was appended since it last read, as a
    /// writer does or as a reader; with `dies`, the cut of what was never
    /// finished ends after that step of it. Then it holds the log to its
    /// end, or to where the log is noted to end for a reader.
    pub fn read_on(&mut self, index: usize, cuts: bool, dies: Option<u8>) {
        self.trace
            .push(format!("handle {index} reads on, as a writer: {cuts}"));
        let handle = &mut self.handles[index];
        alt_oplog::v2::cut_dies_after(dies);
        let read = match cuts {
            true => handle.log.refresh(),
            false => handle.log.follow(),
        };
        alt_oplog::v2::cut_dies_after(None);
        if let (Some(step), Err(OpLogError::Io(_))) = (dies, &read) {
            self.trace.push("  the cut dies".into());
            return self.cut_died(index, step, None);
        }
        if self.took(index, read, false).is_none() {
            return;
        }
        let log = &self.handles[index].log;
        let (held, end) = (log.len(), log.appended_len());
        if cuts {
            self.ledger.begun = None;
        }
        let hidden = held != self.ledger.ops.len();
        assert!(
            self.holds_enough(held, end) && !(cuts && hidden),
            "handle {index} read on and holds {held} of the {} ops of the log, up to {end}",
            self.ledger.ops.len()
        );
        if hidden {
            self.counts.hidden.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn step(&mut self) {
        if self.handles.is_empty() {
            self.open(false);
        }
        self.calls();
        self.lacked = false;
        match self.rng.below(100) {
            0..7 => {
                let read_only = self.rng.one_in(2);
                self.open(read_only);
            }
            7..10 => {
                let gone = self.rng.below(self.handles.len() as u64) as usize;
                self.trace.push(format!("handle {gone} is closed"));
                self.handles.remove(gone);
            }
            10..32 => self.append(),
            scene @ 32..42 => self.scene(scene - 32),
            42..50 => {
                let (index, how) = (self.writer(), self.rng.below(4));
                self.sync(index, how);
            }
            50..51 => {
                let index = self.writer();
                self.drops_a_ticket(index);
            }
            51..58 => {
                let index = self.rng.below(self.handles.len() as u64) as usize;
                let cuts = !self.handles[index].read_only && self.rng.one_in(2);
                self.read_on(index, cuts, None);
            }
            58..67 => self.look(false),
            67..72 => {
                let index = self.writer();
                self.settle(index, None, false);
            }
            72..77 => self.tear(),
            77..83 => self.reboot(false),
            83..88 => {
                let (index, dies) = (self.writer(), 1 + self.rng.below(5) as u8);
                self.settle(index, Some(dies), false);
            }
            88..92 => self.receipt(),
            92..95 => self.open_anchored(),
            95..96 => self.owed_by_a_sink(),
            96..97 => self.sync_unpromised(),
            _ => self.grow_back(),
        }
    }

    /// A reader looks and refuses an op, wherever there is one.
    pub fn look_and_refuse(&mut self) {
        self.look(true);
    }
}
