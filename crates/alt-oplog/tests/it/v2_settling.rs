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
        let len = self.file_len();
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
            // noted beside the log and nothing else: its ops are all there
            1 => {
                let at = self.ledger.ops.iter().find(|op| op.payload == refused);
                let at = at.expect("the op refused is in the log").at;
                self.ledger.begun = Some(at);
                self.ledger.hides.push((len, at));
            }
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
        if cut.is_some() {
            self.counts.cuts.fetch_add(1, Ordering::Relaxed);
        }
        self.settled(&unsettled, &refused, &asked, (cut, told));
        let len = self.file_len();
        self.handles[index].seen.push(len);
        true
    }
}
