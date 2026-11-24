//! The machine goes down under a driven version 2 log. What is on disk
//! after is, stretch by stretch, the file as it was written last or as
//! it was when it was last fsynced: bytes written since may be there or
//! not, a part of them alone may be, and where they were written over
//! what was on disk, that may be there again. The log then holds the
//! ops whose frames end before the first byte that is not as it was
//! written, and past them no op but one that was in the log before, or
//! when the file was last fsynced.

use std::fs;

use super::v2_cut_told::cut_told;
use super::v2_driver::Run;
use super::v2_ledger::{Entry, Ledger};
use super::v2_model::{Reference, reference_of};
use super::v2_power::same_to;

/// What the machine going down takes of what was written since the file
/// was last fsynced.
#[derive(Clone, Copy)]
pub enum Loss {
    /// any stretch of it, or nothing
    Any,
    Nothing,
    /// a stretch that begins within some forty bytes past this one
    Within(u64),
}

impl Ledger {
    /// The machine went down: `log` is the log as its file tells it
    /// now, `then` as the file told it when it was last fsynced, and
    /// the bytes before `same` are as they were written. Holds the ops
    /// of the log to what a loss of power can make of them; how many
    /// are in the log again that were cut off it.
    pub fn after_power(
        &mut self,
        log: &Reference,
        then: &Reference,
        same: u64,
        synced: u64,
    ) -> usize {
        let before = std::mem::take(&mut self.ops);
        let place = |op: &Entry| (op.id, op.at, op.end);
        let mut back = 0;
        for laid in &log.ops {
            let known = self.ever.get(&laid.payload);
            let known = known.expect("an op of the log is one a handle appended");
            let seq = self.ops.len() as u64 + 1;
            assert_eq!(
                ((laid.id, laid.at, laid.end), seq),
                (place(known), known.seq),
                "op {} lies elsewhere once the power went",
                known.n
            );
            let held = before.iter().any(|op| op.id == laid.id);
            let was = then.ops.iter().any(|op| op.id == laid.id);
            assert!(
                held || was,
                "op {} is in the log once the power went: it was in it neither before nor \
                 when the file was last fsynced",
                known.n
            );
            back += !held as usize;
            self.gone.remove(&laid.payload);
            self.ops.push(known.clone());
        }
        let kept = |op: &Entry| self.ops.iter().any(|kept| kept.id == op.id);
        let lost: Vec<Entry> = before.into_iter().filter(|op| !kept(op)).collect();
        for op in &lost {
            assert!(
                op.end > same && op.end > synced,
                "op {}, which ends at {}, is gone once the power went: the file is as it \
                 was up to {same}, and the log synced up to {synced}",
                op.n,
                op.end
            );
        }
        self.gone.extend(lost.iter().map(|op| op.payload.clone()));
        let was = std::mem::take(&mut self.was);
        let was = if lost.is_empty() { was } else { lost };
        self.was = was.into_iter().filter(|op| !kept(op)).collect();
        (self.receipts_of(log), self.hides.clear());
        back
    }
}

impl Ledger {
    /// Holds the receipts of `log`, each where it was appended.
    fn receipts_of(&mut self, log: &Reference) {
        let slips = log.receipts.iter().map(|(end, of)| (self.slips[of], *end));
        let slips = slips.map(|(slip, end)| (slip.3 == end).then_some(slip));
        let slips: Option<_> = slips.collect();
        self.receipts = slips.expect("a receipt of the log lies where it was appended");
    }
}

impl Run {
    /// What is on disk of the file once the machine went down with
    /// `loss`: `before` as it was written, `held` as it was when last
    /// fsynced, the two the same up to `from`. Told with it in words.
    fn on_disk(
        &mut self,
        loss: Loss,
        before: &[u8],
        held: &[u8],
        from: usize,
    ) -> (Vec<u8>, String) {
        let (len, most) = (before.len(), before.len().max(held.len()));
        let within = |at: u64| (from..len).contains(&(at as usize));
        let mut below = |bound: usize| self.rng.below(bound as u64) as usize;
        // the stretch that is as it was when the file was last fsynced
        let old = match (loss, below(6)) {
            (Loss::Nothing, _) => None,
            (Loss::Within(at), _) if within(at) => {
                let start = at as usize + below(48.min(len - at as usize));
                Some((start, (start + 1 + below(40)).min(most)))
            }
            (_, 0) if most > from => Some((from + below(most - from + 1), most)),
            (_, 1 | 2) if most > from => {
                let start = from + below(most - from);
                Some((start, start + 1 + below(most - start)))
            }
            (_, 3) if most > from => Some((from, most)),
            _ => None,
        };
        let mut after = before.to_vec();
        let Some((start, end)) = old.filter(|(start, end)| start < end) else {
            return (after, "nothing is lost".to_owned());
        };
        // a file made shorter since is as long as it was, or it is not
        if end > len && held.len() > len {
            after.extend_from_slice(&held[len..]);
        }
        for at in start..end.min(after.len()) {
            after[at] = held.get(at).copied().unwrap_or(0);
        }
        // what was never on disk does not end the file
        if end >= after.len() {
            after.truncate(start.max(held.len()).min(after.len()));
        }
        let lost = format!(
            "{start}..{end} is as it was when the file of {} bytes was fsynced",
            held.len()
        );
        (after, lost)
    }

    /// The machine goes down: the handles are gone, and of what was
    /// written since the file was last fsynced a part may be, as `loss`
    /// has it.
    pub fn reboot_as(&mut self, loss: Loss) {
        self.synced_now();
        self.all_die();
        let path = self.path().join("log");
        let (before, held) = (fs::read(&path).unwrap(), self.power.held());
        let from = self.durable_to() as usize;
        let (mut after, mut lost) = self.on_disk(loss, &before, &held, from);
        // what the header tells of a cut is synced when the cut begins,
        // and not when a writer takes it back
        let told = |file: &[u8]| file[44..56].to_vec();
        let lossy = !matches!(loss, Loss::Nothing);
        if lossy && told(&held) != told(&after) && self.also.one_in(2) {
            after[44..56].copy_from_slice(&held[44..56]);
            lost += ", and the header tells of a cut as it did then";
        }
        fs::write(&path, &after).unwrap();
        let same = 64 + same_to(&before[64..], &after[64..]) as u64;
        let (log, then) = (self.log(), reference_of(&held).0);
        let back = self
            .ledger
            .after_power(&log, &then, same, self.model.synced);
        self.power.hold(after);
        // a cut the header tells of that the log does not hold
        let (generation, at) = cut_told(self.path());
        let begun = generation > log.cut.0 && at >= log.said_synced.max(64) && at < log.end;
        self.ledger.begun = begun.then_some(at);
        if lossy {
            self.lose_unsynced();
        }
        self.model.rebooted(&log);
        Self::in_boot(self.beside, self.model.boot);
        self.counts
            .back
            .fetch_add(back as u64, std::sync::atomic::Ordering::Relaxed);
        self.trace.push(format!(
            "boot {} of {} bytes: {lost}; {back} ops are in the log again",
            self.model.boot,
            before.len()
        ));
    }
}
