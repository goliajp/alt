//! What a driven sequence holds the syncs of its handles to: no sync says
//! of an op that was cut that it is synced. Each sync a handle leaves to
//! its sink is kept account of, and comes to one of two ends: the sink
//! syncs it, with its ops in the log and within what is synced, or the
//! sink tells of its cut, once. And a sync that says the ops of a handle
//! are synced finds every op the handle appended in the log.

use std::sync::atomic::Ordering;

use alt_oplog::{Anchor, OpLogError};

use super::v2_driver::Run;
use super::v2_model::Laid;

impl Run {
    /// Whether the op at `last` is gone from the log, or a cut of frames
    /// before byte `end` was begun.
    fn cut(&self, (last, end): (Option<Anchor>, u64)) -> bool {
        !self.ledger.holds(last) || self.ledger.begun.is_some_and(|at| at < end)
    }

    /// Up to where the log is synced now, as far as the driver knows.
    fn synced_now(&mut self) -> u64 {
        self.calls();
        let log = self.log();
        let claimed = self.claimed(&log);
        self.model.synced_to(&log, claimed);
        self.model.synced
    }

    /// Handle `index` was told of the ops it appended that are gone, or
    /// its caller knows of them.
    pub fn forget_gone(&mut self, index: usize) {
        let ledger = &self.ledger;
        let handle = &mut self.handles[index];
        handle.own.retain(|own| ledger.holds(Some(*own)));
        handle.receipts.retain(|of| ledger.receipt_of(*of));
    }

    /// A sync said that what handle `index` holds is synced: every op
    /// the handle appended that no sync has answered for is in the log.
    fn own_synced(&mut self, index: usize) {
        for own in std::mem::take(&mut self.handles[index].own) {
            assert!(
                self.ledger.holds(Some(own)),
                "a sync of handle {index} says its ops are synced: the op it appended at {} \
                 is not in the log",
                own.start
            );
        }
        for of in std::mem::take(&mut self.handles[index].receipts) {
            assert!(
                self.ledger.receipt_of(of),
                "a sync of handle {index} says its frames are synced: a receipt it appended \
                 is not in the log"
            );
        }
    }

    /// Takes in what the sink of handle `index` answered to an fsync:
    /// every sync left to it is synced, or it tells of a cut of them.
    fn sink_answered(&mut self, index: usize, answered: &Result<(), OpLogError>) {
        match answered {
            Ok(()) => {
                let synced = self.synced_now();
                for (last, end) in std::mem::take(&mut self.handles[index].left) {
                    assert!(
                        self.ledger.holds(last),
                        "the sink of handle {index} says a sync left to it is synced: its \
                         ops were cut"
                    );
                    assert!(
                        end <= synced,
                        "the sink of handle {index} says a sync up to {end} is synced: the \
                         log is up to {synced}"
                    );
                    self.counts.left_synced.fetch_add(1, Ordering::Relaxed);
                }
                self.own_synced(index);
            }
            Err(OpLogError::CutBack) => {
                let left = std::mem::take(&mut self.handles[index].left);
                assert!(
                    !left.is_empty(),
                    "the sink of handle {index} was left nothing to sync"
                );
                assert!(
                    left.iter().any(|left| self.cut(*left)),
                    "the sink of handle {index} tells of a cut: the log holds what it was left"
                );
                let told = &self.counts.left_told;
                told.fetch_add(left.len() as u64, Ordering::Relaxed);
                self.forget_gone(index);
            }
            Err(_) => {}
        }
    }

    /// Has handle `index` sync what it holds, or set a boundary over the
    /// log with `how` not zero; a sink it has then answers for what was
    /// left to it. What another handle did meanwhile is taken in before
    /// the answers are held to the log; `unsettled` are the ops that
    /// were left to look at before. Gives what the handle itself is to
    /// take in.
    pub fn sync_through(
        &mut self,
        index: usize,
        how: u64,
        unsettled: &[Laid],
    ) -> Result<(), OpLogError> {
        let handle = &mut self.handles[index];
        let asked = match how {
            0 => handle.log.sync(),
            _ => handle.log.bound_now(),
        };
        if handle.sink.is_none() {
            self.meanwhile_done(unsettled);
            if asked.is_ok() {
                self.own_synced(index);
            }
            return asked;
        }
        if how == 0 && asked.is_ok() {
            handle.leaves();
        }
        self.meanwhile_done(unsettled);
        // a handle is told of a cut, and nothing new is left to its sink,
        // while the sink still owes an answer for what was cut
        let told = matches!(asked, Err(OpLogError::CutBack));
        let left = &self.handles[index].left;
        let owed = told && left.iter().any(|left| self.cut(*left));
        if !asked.is_ok() && !owed {
            return asked;
        }
        let answered = self.handles[index].sink.as_ref().unwrap().fsync();
        self.meanwhile_done(unsettled);
        self.sink_answered(index, &answered);
        if asked.is_ok() {
            self.by_sink = true;
            return answered;
        }
        assert!(
            matches!(answered, Err(OpLogError::CutBack)),
            "handle {index} is told of a cut for what its sink owes, and the sink answers \
             {answered:?}"
        );
        self.counts.refused_over.fetch_add(1, Ordering::Relaxed);
        self.owed_told = true;
        asked
    }

    /// Handle `index` is told that its ops were cut, by a call on the
    /// handle itself: that is so where ops it holds are gone or a cut of
    /// them was begun, or where an op it appended that no sync answered
    /// for is gone.
    pub fn handle_told(&mut self, index: usize) {
        let handle = &self.handles[index];
        let (last, end) = (handle.log.anchor(), handle.log.appended_len());
        let begun = self.ledger.begun.is_some_and(|at| at < end);
        let held = self.ledger.holds(last);
        let lost = handle.own.iter().any(|own| !self.ledger.holds(Some(*own)))
            || handle
                .receipts
                .iter()
                .any(|of| !self.ledger.receipt_of(*of));
        assert!(
            begun || !held || lost,
            "handle {index} is told its ops were cut: the log holds them, up to {end}, and \
             the {} ops it appended since a sync answered for them",
            handle.own.len()
        );
        let of = match (held, begun) {
            (true, true) => &self.counts.cut_back_begun,
            (true, false) => &self.counts.lost_told,
            (false, _) => return,
        };
        of.fetch_add(1, Ordering::Relaxed);
    }
}
