//! A writer of a driven version 2 log looks at what was left and cuts
//! what nobody vouches for, and a cut that dies at one of its steps.

use std::sync::atomic::Ordering;

use alt_oplog::OpLogError;

use super::v2_driver::{Run, name};
use super::v2_meanwhile::{refused_of, says};

impl Run {
    /// The cut of a call on a handle ended after `step` of its steps as
    /// if the process had died: the handle is gone. `refused` is the op
    /// the cut was of, when it was of ops nobody vouches for.
    pub fn cut_died(&mut self, index: usize, step: u8, refused: Option<&[u8]>) {
        self.counts.cuts_died.fetch_add(1, Ordering::Relaxed);
        self.died += 1;
        let begun_at = super::v2_cut_told::cut_told(self.path()).1;
        self.ledger.died_at.push(begun_at);
        self.dies(index);
        self.ledger.hides.clear();
        self.ledger.begun = None;
        let Some(refused) = refused else {
            return;
        };
        match step {
            // told in the header of the log and nothing else: its ops
            // are all there
            1 => self.ledger.begun = Some(self.ledger.place_of(refused)),
            // the checkpoint lies over the first of them
            _ => drop(self.cut_took(refused)),
        }
    }

    /// A writer looks at what was left, as it does before it appends, and
    /// cuts what is not vouched for; with `surely`, what an op names is
    /// gone first, wherever there is one that may be. With `dies`, a cut
    /// ends after that step of it as if the process had died.
    pub fn settle(&mut self, index: usize, dies: Option<u8>, surely: bool) -> bool {
        let unsettled = self.unsettled_before(u64::MAX);
        if surely || self.rng.one_in(3) {
            self.lose_one();
        }
        self.trace
            .push(format!("handle {index} settles, dies: {dies:?}"));
        let (stored, mut asked) = (self.model.stored.clone(), Vec::new());
        alt_oplog::v2::cut_dies_after(dies);
        let cut = self.handles[index].log.settle(says(&stored, &mut asked));
        alt_oplog::v2::cut_dies_after(None);
        let refused = refused_of(&asked);
        if let (Some(step), Err(OpLogError::Io(_))) = (dies, &cut) {
            // what it was told before it died it was told
            self.trace.push(format!(
                "  the cut dies, of {:?}",
                refused.as_deref().map(name)
            ));
            let vouched = asked.iter().take_while(|(_, held)| *held);
            self.model
                .vouched
                .extend(vouched.map(|(payload, _)| payload.clone()));
            self.cut_died(index, step, refused.as_deref());
            return false;
        }
        let told = self.handles[index].log.take_discarded();
        if let Err(why @ (OpLogError::Kept { .. } | OpLogError::Lost { .. })) = &cut {
            self.cut_refused(&refused, &asked, why, &told);
            return false;
        }
        let Some(cut) = self.took(index, cut, false) else {
            return false;
        };
        let had = self.in_the_log(index);
        let cut = self.settled(&unsettled, &asked, (cut, told));
        self.counts.cuts.fetch_add(cut as u64, Ordering::Relaxed);
        // of the frames on no ticket that it cut itself, its caller is
        // told by the look, and of no other
        self.forget_cut_now(index, had);
        let len = self.file_len();
        let handle = &mut self.handles[index];
        handle.seen.push(len);
        handle.looked = true;
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
        self.comes_upon(held, step);
    }

    /// Handle `held` holds ops past where a cut died after `step`, and
    /// does what finds the cut told in the log.
    fn comes_upon(&mut self, held: usize, step: u8) {
        let way = self.rng.below(4);
        self.trace.push(format!(
            "handle {held} comes upon the cut that died after step {step} (way {way})"
        ));
        let handle = &mut self.handles[held];
        let came = &self.counts.came_upon[step as usize - 1];
        came.fetch_add(1, Ordering::Relaxed);
        // whatever reads the log tells of the cut; a sync of a handle
        // that wrote none of the ops reads the log again by itself
        if way < 2 {
            let did = match way {
                0 => handle.log.follow(),
                _ => handle.log.refresh(),
            };
            assert!(
                matches!(did, Err(OpLogError::CutBack)),
                "a handle that holds ops past where a cut died after step {step} is told {did:?}"
            );
            self.took(held, did, false);
            return;
        }
        let synced = self.sync_through(held, way - 2, &[]);
        self.by_sink = false;
        match synced {
            Err(OpLogError::Unjudged { at }) => self.unjudged(held, at),
            // what an op it has seen to names is gone
            Err(OpLogError::Io(_)) if self.lacked_now() => return,
            _ => {}
        }
        // one that leaves its syncs to a sink learns nothing by that
        let handle = &self.handles[held];
        let told = matches!(synced, Err(OpLogError::CutBack));
        let holds = handle.sink.is_some() || self.ledger.holds(handle.log.anchor());
        assert!(
            !told && holds,
            "a handle past where a cut died after step {step} syncs: {synced:?}"
        );
    }
}
