//! After every step of a driven sequence: the log, the note beside it,
//! every handle and two opened now are held to what the file and the
//! driver know.

use alt_oplog::OpLog;

use super::v2_driver::{Run, name};
use super::v2_ledger::Entry;
use super::v2_model::{Laid, Model};

/// Holds `handle` to the model: it takes the log for synced no further,
/// and has ops to look at while one it holds was left by another boot
/// and never vouched for.
fn held_to(handle: &OpLog, unsettled: &[&Laid], model: &Model, who: &str) {
    let bounded = handle.bounded_to().unwrap_or(0);
    assert!(
        bounded <= model.synced,
        "{who} takes the log for synced up to {bounded}: it is up to {}",
        model.synced
    );
    let end = handle.appended_len();
    if let Some(op) = unsettled.iter().find(|op| op.at < end) {
        assert!(
            handle.unsettled(),
            "{who} has nothing to look at: {} at {} was left by another boot \
             and nobody vouched for it",
            name(&op.payload),
            op.at
        );
    }
}

impl Run {
    /// Whether a handle that holds `held` ops of the log up to byte `end`
    /// holds as many as it may: all of them, those before a place the
    /// log is noted to end at in a file as long as this one, or those
    /// before where the log tells of a cut that never came to it.
    pub fn holds_enough(&self, held: usize, end: u64) -> bool {
        let hidden = (self.file_len(), end);
        held == self.ledger.ops.len()
            || self.ledger.hides.contains(&hidden)
            || self.ledger.begun == Some(end)
    }

    /// Holds a handle opened now, as a reader and as a writer, to the
    /// model and the ledger: it holds the ops of the log from the first
    /// one on, all of them unless the log is noted to end before.
    fn opened_now(&self, unsettled: &[&Laid]) {
        for read_only in [true, false] {
            let opened = match read_only {
                true => OpLog::open_read_only(self.path(), None),
                false => OpLog::open(self.path()),
            };
            let fresh = opened.expect("the log opens");
            held_to(&fresh, unsettled, &self.model, "a handle opened now");
            let ops = fresh.ops().expect("the log reads from its first frame");
            let same = |(held, known): (&alt_oplog::Op, &Entry)| held.id == known.id;
            assert!(ops.len() <= self.ledger.ops.len());
            assert!(ops.iter().zip(&self.ledger.ops).all(same));
            assert!(
                ops.len() == self.ledger.ops.len() || fresh.passed_over(),
                "a handle opened now holds fewer ops than the log and tells of nothing past them"
            );
            assert!(
                self.holds_enough(ops.len(), fresh.appended_len()),
                "a handle opened now holds {} of the {} ops of the log, up to {}: \
                 nothing says the log ends there in a file of {} bytes",
                ops.len(),
                self.ledger.ops.len(),
                fresh.appended_len(),
                self.file_len()
            );
            fresh.verify().expect("the log holds to the full check");
        }
    }

    /// Holds the log, the note beside it, every handle and two opened
    /// now to the model and the ledger, after a step.
    pub fn held_to_the_model(&mut self) {
        self.calls();
        let log = self.log();
        self.ledger.agrees(&log);
        // a cut the header tells of no longer once a writer has read
        // the log to its end, as a sync does for a handle whose frames
        // are gone
        if super::v2_cut_told::cut_told(self.path()).0 <= log.cut.0 {
            self.ledger.begun = None;
        }
        // the log says of no cut that it took ops but of one that did,
        // or of one made again where a cut had died
        for cut in &log.cuts {
            let before = self.ledger.before(*cut);
            let died = |at: &u64| self.ledger.before(*at) == before;
            assert!(
                self.cut_seen(*cut) || self.ledger.died_at.iter().any(died),
                "the log says a cut at {cut} took ops: none was made of ops or died there"
            );
        }
        let claimed = self.claimed(&log);
        self.model.synced_to(&log, claimed);
        // no boundary is recorded over bytes a loss of power could take
        let durable = self.durable_to();
        assert!(
            self.model.synced <= durable,
            "the log is recorded as synced up to {}: it is fsynced, as it is now, up to {durable}",
            self.model.synced
        );
        assert!(
            log.said_synced <= self.model.synced,
            "a frame says the log was synced up to {}: it is up to {}",
            log.said_synced,
            self.model.synced
        );
        let unsettled = self.model.unsettled(&log);
        for (index, handle) in self.handles.iter().enumerate() {
            if self.ledger.holds(handle.log.anchor()) {
                held_to(
                    &handle.log,
                    &unsettled,
                    &self.model,
                    &format!("handle {index}"),
                );
            }
        }
        self.opened_now(&unsettled);
    }
}
