//! Callers of a driven sequence that had a handle write for them on a
//! ticket ask what became of those frames, at any time after, and at
//! the latest when the sequence is over (`v2_owed`). A ticket may be
//! dropped instead: its frames are the handle's then. One that was told
//! of a cut is told so whenever it asks again.

use std::sync::atomic::Ordering;

use alt_oplog::{Answer, OpLog, OpLogError, Ticket};

use super::v2_beside::Beside;
use super::v2_driver::{Run, name};
use super::v2_meanwhile::{refused_of, says};
use super::v2_model::Laid;
use super::v2_owed::{Claim, Wrote};

impl Run {
    /// Handle `index` wrote `frame`: on `ticket`, for a caller of its
    /// own, or for the caller the handle has without one.
    pub fn wrote(&mut self, index: usize, frame: Wrote, ticket: Option<Ticket>) {
        let handle = &mut self.handles[index];
        match ticket {
            Some(ticket) => {
                assert!(!ticket.is_empty(), "a frame was written on no ticket");
                let frames = vec![frame];
                handle.claims.push(Claim { ticket, frames });
                self.counts.claims.fetch_add(1, Ordering::Relaxed);
            }
            None => handle.mine.push(frame),
        }
    }

    /// The holder of a ticket of handle `index` drops it: whoever asks
    /// the handle without a ticket is answered for its frames.
    pub fn drops_a_ticket(&mut self, index: usize) {
        let claims = self.handles[index].claims.len() as u64;
        if claims == 0 {
            return;
        }
        let claim = self.rng.below(claims) as usize;
        let handle = &mut self.handles[index];
        let mut dropped = handle.claims.remove(claim);
        self.trace.push(format!(
            "a ticket of handle {index} for {} frames is dropped",
            dropped.frames.len()
        ));
        handle.mine.append(&mut dropped.frames);
        self.counts.claims_dropped.fetch_add(1, Ordering::Relaxed);
    }

    /// What the holder of ticket `claim` of handle `index` is answered:
    /// by the handle, and where that leaves the frames to its sink, by
    /// the sink, which fsyncs for it or after an fsync made for all.
    /// Where the sink has nothing to sync the frames up to, the handle
    /// is asked once more, syncs them itself, and its answer is final.
    fn answered(
        &mut self,
        index: usize,
        claim: usize,
        unsettled: &[Laid],
    ) -> Result<(), OpLogError> {
        let handle = &mut self.handles[index];
        handle.log.sync_for(&handle.claims[claim].ticket)?;
        if handle.sink.is_none() {
            return Ok(());
        }
        if self.also.one_in(2) {
            // the fsync a leader makes, which answers its caller without
            // a ticket as well
            let _ = self.sink_asked(index, unsettled);
            self.by_sink = false;
        }
        let handle = &mut self.handles[index];
        let (sink, ticket) = (handle.sink.as_ref().unwrap(), &handle.claims[claim].ticket);
        let answer = match self.also.one_in(2) {
            true => sink.answer(ticket),
            false => sink.fsync_for(ticket),
        };
        if answer? == Answer::Synced {
            return Ok(());
        }
        self.counts.claims_owed.fetch_add(1, Ordering::Relaxed);
        self.trace.push("  its sink owes it an answer".into());
        handle.log.sync_for(ticket)?;
        let after = sink.answer(ticket);
        assert!(
            matches!(after, Ok(Answer::Synced)),
            "handle {index} synced for a ticket its sink owed an answer: the sink says {after:?}"
        );
        Ok(())
    }

    /// The holder of ticket `claim` of handle `index` asks. What another
    /// handle did meanwhile is taken in before the answer is held to
    /// the log. Whether it was answered.
    pub fn ask(&mut self, index: usize, claim: usize, unsettled: &[Laid]) -> bool {
        let who = format!("handle {index}, asked with a ticket,");
        let before = (self.handles[index].log.reads(), self.file_len());
        let answer = self.answered(index, claim, unsettled);
        self.read_anew_since(index, before);
        self.meanwhile_done(unsettled);
        self.trace
            .push(format!("  its ticket is answered {answer:?}"));
        let frames = self.handles[index].claims[claim].frames.clone();
        match answer {
            Ok(()) => {
                self.told_synced(&frames, &who);
                self.counts.claims_synced.fetch_add(1, Ordering::Relaxed);
            }
            Err(OpLogError::CutBack) => {
                self.told_cut(&frames, &who);
                self.counts.claims_cut.fetch_add(1, Ordering::Relaxed);
            }
            Err(OpLogError::Io(_)) => return false,
            Err(OpLogError::Unjudged { at }) => {
                self.unjudged(index, at);
                return false;
            }
            Err(other) => panic!("{other:?}"),
        }
        let handle = &mut self.handles[index];
        let answered = handle.claims.remove(claim);
        if answer.is_err() {
            handle.spent.push(answered.ticket);
        }
        true
    }

    /// A ticket of handle `index` that was told of a cut asks again,
    /// the handle or its sink: it is told so again.
    pub fn asks_again(&mut self, index: usize) {
        let handle = &mut self.handles[index];
        let Some(ticket) = handle.spent.last() else {
            return;
        };
        let sink = handle.sink.as_ref();
        let again = match (self.also.below(3), sink) {
            (1, Some(sink)) => sink.answer(ticket).map(drop),
            (2, Some(sink)) => sink.fsync_for(ticket).map(drop),
            _ => handle.log.sync_for(ticket),
        };
        assert!(
            matches!(again, Err(OpLogError::CutBack)),
            "a ticket of handle {index} that was told of a cut asks again: {again:?}"
        );
        self.counts.claims_again.fetch_add(1, Ordering::Relaxed);
    }

    /// Every caller that holds a ticket of handle `index` asks, and the
    /// one without: at the end of a sequence, however long ago the
    /// frames were written.
    pub fn all_ask(&mut self, index: usize) {
        let mut unanswered = 0;
        while self.handles[index].claims.len() > unanswered {
            self.trace.push(format!("handle {index} is asked at last"));
            match self.ask(index, unanswered, &[]) {
                true => drop(self.counts.claims_late.fetch_add(1, Ordering::Relaxed)),
                false => unanswered += 1,
            }
        }
        self.asks_again(index);
        if !self.handles[index].read_only {
            self.trace.push(format!("handle {index} syncs at last"));
            // told of a cut once, and then not
            if let Err(OpLogError::CutBack) = self.sync_through(index, 0, &[]) {
                self.by_sink = false;
                let again = self.sync_through(index, 0, &[]);
                let told = matches!(again, Err(OpLogError::CutBack));
                assert!(!told, "handle {index} is told twice of a cut");
            }
            self.by_sink = false;
        }
    }

    /// A caller whose op is in the log is owed an answer by a sink that
    /// has nothing to sync the op up to: the handle read an op another
    /// writer appended after and left the sink the place past that one,
    /// which is then cut. The caller has the handle leave its op anew.
    /// Where the note beside the log is kept, nobody looks at an op of
    /// the running boot, and none is cut.
    pub fn owed_by_a_sink(&mut self) {
        if self.beside == Beside::Kept {
            return;
        }
        self.to_defer = true;
        let index = self.open(false);
        self.to_defer = false;
        self.settle(index, None, false);
        let (meta, payload) = self.op();
        self.to_ticket = true;
        if !self.put(index, (meta, payload), false, false) {
            return;
        }
        let handle = &mut self.handles[index];
        let claim = handle.claims.len() - 1;
        if handle.log.sync_for(&handle.claims[claim].ticket).is_err() {
            return;
        }
        // another writer appends an op, which nothing is promised for
        let (meta, after) = self.op();
        let mut other = OpLog::open(self.path()).expect("the log opens");
        let unsynced = alt_os::durability::leave_unsynced(true);
        let appended = other.append_with("driver", 1, &after, &meta);
        drop(unsynced);
        if appended.is_err() {
            return;
        }
        self.trace
            .push(format!("another writer appends {}", name(&after)));
        self.appended(
            "driver",
            after.clone(),
            (&meta, true),
            other.anchor().unwrap(),
        );
        drop(other);
        let handle = &mut self.handles[index];
        let read = handle.log.follow();
        let left = handle.log.sync_for(&handle.claims[claim].ticket);
        if read.is_err() || left.is_err() {
            return;
        }
        // what that op names is gone, and a writer that finds no note
        // beside the log looks
        let unsettled = self.unsettled_before(u64::MAX);
        self.model.stored.lock().unwrap().remove(&after);
        let _ = std::fs::remove_file(self.path().join("tail"));
        let mut cutter = OpLog::open(self.path()).expect("the log opens");
        let (stored, mut asked) = (self.model.stored.clone(), Vec::new());
        let cut = cutter.settle(says(&stored, &mut asked));
        let told = cutter.take_discarded();
        drop(cutter);
        self.trace.push("a writer looks".into());
        match cut {
            Ok(cut) => drop(self.settled(&unsettled, &asked, (cut, told))),
            Err(why @ (OpLogError::Kept { .. } | OpLogError::Lost { .. })) => {
                return self.cut_refused(&refused_of(&asked), &asked, &why, &told);
            }
            Err(other) => panic!("{other:?}"),
        }
        self.ask(index, claim, &[]);
    }
}
