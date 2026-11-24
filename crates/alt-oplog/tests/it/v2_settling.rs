//! A writer of a driven version 2 log looks at what was left and cuts
//! what nobody vouches for, and a cut that dies at one of its steps.

use std::sync::atomic::Ordering;

use alt_oplog::OpLogError;

use super::v2_driver::{Run, name};
use super::v2_meanwhile::says;

impl Run {
    /// The cut of a call on a handle ended after `step` of its steps as
    /// if the process had died: the handle is gone. `refused` is the op
    /// the cut was of, when it was of ops nobody vouches for.
    pub fn cut_died(&mut self, index: usize, step: u8, refused: Option<&[u8]>) {
        self.counts.cuts_died.fetch_add(1, Ordering::Relaxed);
        self.died += 1;
        self.handles.remove(index);
        self.ledger.hides.clear();
        self.ledger.begun = None;
        // its checkpoint is synced from the third step on
        self.cut_synced = step >= 3;
        let Some(refused) = refused else {
            return;
        };
        match step {
            // told in the header of the log and nothing else: its ops
            // are all there
            1 => self.ledger.begun = Some(self.ledger.place_of(refused)),
            // the checkpoint lies over the first of them
            _ => drop(self.ledger.cut_from(refused, self.model.synced)),
        }
    }

    /// A writer looks at what was left, as it does before it appends, and
    /// cuts what is not vouched for; with `surely`, it refuses an op
    /// wherever there is one. With `dies`, a cut ends after that step of
    /// it as if the process had died.
    pub fn settle(&mut self, index: usize, dies: Option<u8>, surely: bool) -> bool {
        let unsettled = self.unsettled_before(u64::MAX);
        let refused = self.refusal(surely);
        let refuses = refused.as_deref().map(name);
        self.trace.push(format!(
            "handle {index} settles, refusing {refuses:?}, dies: {dies:?}"
        ));
        let mut asked = Vec::new();
        alt_oplog::v2::cut_dies_after(dies);
        let cut = self.handles[index].log.settle(says(&refused, &mut asked));
        alt_oplog::v2::cut_dies_after(None);
        if let (Some(step), Err(OpLogError::Io(_))) = (dies, &cut) {
            // what it was told before it died it was told
            self.trace.push("  the cut dies".into());
            let vouched = asked.iter().filter(|(_, held)| *held);
            self.model
                .vouched
                .extend(vouched.map(|(payload, _)| payload.clone()));
            let cut_of = asked.iter().any(|(_, held)| !held);
            self.cut_died(index, step, refused.as_deref().filter(|_| cut_of));
            return false;
        }
        let told = self.handles[index].log.take_discarded();
        let Some(cut) = self.took(index, cut, false) else {
            return false;
        };
        let cut = self.settled(&unsettled, &refused, &asked, (cut, told));
        self.counts.cuts.fetch_add(cut as u64, Ordering::Relaxed);
        let len = self.file_len();
        self.handles[index].seen.push(len);
        true
    }

    /// A cut dies after one of its steps, any of them, and a handle that
    /// holds ops past where it cut does what finds the cut told in the
    /// log: it is told its ops were cut. After the first step the log
    /// holds them still, and tells of the cut in its header alone.
    pub fn cut_begun(&mut self) {
        // what is not synced is another boot's to look at
        for _ in 0..2 {
            let (index, op) = (self.writer(), self.op());
            self.put(index, op, true, false);
        }
        self.reboot(true);
        let held = self.open(false);
        let cutter = self.open(false);
        let (step, died) = (1 + self.also.below(5) as u8, self.died);
        let (ops, held_to) = (self.ledger.ops.len(), self.handles[held].log.appended_len());
        self.settle(cutter, Some(step), true);
        // it died, of a cut of ops, at a place the handle holds ops
        // past: the log may have ended for it before, where an earlier
        // cut was begun and never made
        let cut = self
            .ledger
            .was
            .first()
            .filter(|_| self.ledger.ops.len() < ops);
        let at = self.ledger.begun.or(cut.map(|op| op.at));
        if self.died == died || at.is_none_or(|at| at >= held_to) {
            return;
        }
        let way = self.rng.below(4);
        self.trace.push(format!(
            "handle {held} comes upon the cut that died after step {step} (way {way})"
        ));
        let handle = &mut self.handles[held];
        let mut did = match way {
            0 => handle.log.follow(),
            1 => handle.log.refresh(),
            2 => handle.log.sync(),
            _ => handle.log.bound_now(),
        };
        if let (2, Ok(()), Some(sink)) = (way, &did, &handle.sink) {
            did = sink.fsync();
            self.by_sink = true;
            self.handles[held].leaves();
        }
        assert!(
            matches!(did, Err(OpLogError::CutBack)),
            "a handle that holds ops past where a cut died after step {step} is told {did:?}"
        );
        let came = &self.counts.came_upon[step as usize - 1];
        came.fetch_add(1, Ordering::Relaxed);
        self.took(held, did, false);
    }
}
