//! Callers of a driven sequence that hold a ticket for the frames a
//! handle wrote for them ask what became of those frames, at any time
//! after, and at the latest when the sequence is over (`v2_owed`).

use std::sync::atomic::Ordering;

use alt_oplog::{Answer, OpLog, OpLogError};

use super::v2_beside::Beside;
use super::v2_driver::{Run, name};
use super::v2_meanwhile::{refused_of, says};
use super::v2_model::Laid;
use super::v2_owed::Claim;

impl Run {
    /// Takes a ticket from handle `index`, for the frames it wrote that
    /// are on none.
    pub fn claim(&mut self, index: usize) {
        let handle = &mut self.handles[index];
        let ticket = handle.log.ticket();
        let mut frames = std::mem::take(&mut handle.mine);
        frames.append(&mut handle.left);
        self.trace.push(format!(
            "handle {index} gives a ticket for {} frames",
            frames.len()
        ));
        handle.claims.push(Claim { ticket, frames });
        self.counts.claims.fetch_add(1, Ordering::Relaxed);
    }

    /// The holder of ticket `claim` of handle `index` asks: the handle,
    /// and its sink where the handle leaves the frames to it. What
    /// another handle did meanwhile is taken in before the answer is
    /// held to the log. Whether it was answered.
    pub fn ask(&mut self, index: usize, claim: usize, unsettled: &[Laid]) -> bool {
        let who = format!("handle {index}, asked with a ticket,");
        for round in 0..3 {
            let before = (self.handles[index].log.reads(), self.file_len());
            let handle = &mut self.handles[index];
            let asked = handle.log.sync_for(&handle.claims[claim].ticket);
            let mut answer = asked.map(|()| Answer::Synced);
            if let (Ok(_), Some(sink)) = (&answer, &handle.sink) {
                answer = sink.fsync_for(&handle.claims[claim].ticket);
            }
            self.read_anew_since(index, before);
            self.meanwhile_done(unsettled);
            self.trace
                .push(format!("  its ticket is answered {answer:?}"));
            let frames = self.handles[index].claims[claim].frames.clone();
            match answer {
                Ok(Answer::Synced) => {
                    self.told_synced(&frames, &who);
                    self.counts.claims_synced.fetch_add(1, Ordering::Relaxed);
                }
                Err(OpLogError::CutBack) => {
                    self.told_cut(&frames, &who);
                    self.counts.claims_cut.fetch_add(1, Ordering::Relaxed);
                }
                // in the log, with what the sink was left cut past them:
                // the handle leaves them anew
                Ok(Answer::Owed) => {
                    assert!(round < 2, "{who} is owed an answer by its sink for good");
                    self.counts.claims_owed.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                Err(OpLogError::Io(_)) => return false,
                Err(OpLogError::Unjudged { at }) => {
                    self.unjudged(index, at);
                    return false;
                }
                Err(other) => panic!("{other:?}"),
            }
            self.handles[index].claims.remove(claim);
            return true;
        }
        unreachable!()
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
        if !self.put(index, (meta, payload), false, false) {
            return;
        }
        self.claim(index);
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
