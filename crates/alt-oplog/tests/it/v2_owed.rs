//! What a driven sequence holds the syncs of its handles to, frame by
//! frame and caller by caller. Every frame a handle wrote is some
//! caller's: the one that holds the ticket it is on, or, on no ticket,
//! the one caller a handle has without tickets. Of each answer a caller
//! gets, by the handle or by its sink:
//!
//! - told that its frames are synced, every one of them is in the log,
//!   within what is synced: no frame that was cut is forgotten, however
//!   long its holder took to ask;
//! - told of a cut, one of its frames is gone from the log: nobody is
//!   told for the frames of another, nor for a ticket nobody asks with.

use std::sync::atomic::Ordering;

use alt_oplog::{Anchor, OpLogError, Ticket};

use super::v2_beside::Beside;
use super::v2_driver::Run;
use super::v2_model::Laid;

/// A frame a handle wrote: an op, or the receipt of a request and where
/// it ends.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Wrote {
    Op(Anchor),
    Receipt([u8; 16], u64),
}

impl Wrote {
    pub fn end(&self) -> u64 {
        match self {
            Wrote::Op(anchor) => anchor.end,
            Wrote::Receipt(_, end) => *end,
        }
    }
}

/// A ticket taken from a handle, and the frames on it.
pub struct Claim {
    pub ticket: Ticket,
    pub frames: Vec<Wrote>,
    /// its writer was told to leave what it writes unsynced
    pub unsynced: bool,
}

impl Run {
    /// Whether the log holds `frame`.
    pub fn has(&self, frame: &Wrote) -> bool {
        match frame {
            Wrote::Op(anchor) => self.ledger.holds(Some(*anchor)),
            Wrote::Receipt(of, _) => self.ledger.receipt_of(*of),
        }
    }

    pub fn gone(&self, frames: &[Wrote]) -> bool {
        frames.iter().any(|frame| !self.has(frame))
    }

    /// Up to where the log is synced now, as far as the driver knows.
    fn synced_now(&mut self) -> u64 {
        self.calls();
        let log = self.log();
        let claimed = self.claimed(&log);
        self.model.synced_to(&log, claimed);
        self.model.synced
    }

    /// The caller of `frames` was told by `who` that they are synced.
    pub fn told_synced(&mut self, frames: &[Wrote], who: &str) {
        let synced = self.synced_now();
        for frame in frames {
            assert!(
                self.has(frame),
                "{who} says the frames of a caller are synced: {frame:?} is not in the log"
            );
            assert!(
                frame.end() <= synced,
                "{who} says {frame:?} is synced: the log is up to {synced}"
            );
        }
    }

    /// The caller of `frames` was told by `who` that some were cut.
    pub fn told_cut(&self, frames: &[Wrote], who: &str) {
        assert!(
            self.gone(frames),
            "{who} tells a caller of a cut: the log holds its {} frames",
            frames.len()
        );
    }

    /// Handle `index` may have read the log anew by itself since it had
    /// read it `before.0` times, in a file `before.1` bytes long then or
    /// as long as it is now.
    pub fn read_anew_since(&mut self, index: usize, before: (u64, u64)) {
        let len = self.file_len();
        let handle = &mut self.handles[index];
        if handle.log.reads() != before.0 {
            self.trace.push("  it reads the log anew by itself".into());
            handle.seen.extend([before.1, len]);
        }
    }

    /// Handle `index` was refused a boundary over the op at `at`, which
    /// it takes for one nobody looked at: the op is in the log, and the
    /// handle has not looked at all that was left itself. A handle may
    /// know less than was done, and ask for a look where the driver
    /// knows of nothing left to look at: of an op it read when it
    /// opened the log, only the note beside the log tells which boot
    /// left it and whether somebody looked, and that note may be gone,
    /// speak of a cut that never was made, or not say what a reader
    /// found: a reader that vouched for all it read notes nothing where
    /// ops were noted as left.
    pub fn unjudged(&mut self, index: usize, at: u64) {
        self.trace
            .push(format!("  no boundary is set over the op at {at}"));
        let log = self.log();
        let left = self.model.unsettled(&log);
        let known = self.ledger.ops.iter().any(|op| op.at == at);
        assert!(
            known,
            "handle {index} is refused a boundary over no op, at {at}"
        );
        assert!(
            !self.handles[index].looked,
            "handle {index}, which looked at all that was left, is refused a boundary over \
             the op at {at}"
        );
        if left.iter().all(|op| op.at != at) {
            let more = match self.beside == Beside::Kept {
                true => &self.counts.unjudged_more_noted,
                false => &self.counts.unjudged_more,
            };
            more.fetch_add(1, Ordering::Relaxed);
        }
        self.counts.unjudged.fetch_add(1, Ordering::Relaxed);
        self.to_look = true;
    }

    /// Handle `index` syncs for its caller without a ticket (`how` 0),
    /// or sets a boundary over the log (`how` 1): that caller is
    /// answered for the frames on no ticket, by the handle or, where
    /// the handle leaves them to its sink, by the sink. What another
    /// handle did meanwhile is taken in before the answers are held to
    /// the log; `unsettled` are the ops that were left to look at
    /// before. Gives what the caller is told. A sync that is left to the
    /// sink answers for nothing itself; a boundary answers for what was
    /// not left.
    pub fn sync_through(
        &mut self,
        index: usize,
        how: u64,
        unsettled: &[Laid],
    ) -> Result<(), OpLogError> {
        let who = format!("handle {index}, asked without a ticket,");
        let before = (self.handles[index].log.reads(), self.file_len());
        let handle = &mut self.handles[index];
        let deferred = handle.sink.is_some();
        let asked = match how {
            0 => handle.log.sync(),
            _ => handle.log.bound_now(),
        };
        let leaves = deferred && how == 0;
        let mut frames = std::mem::take(&mut handle.mine);
        if !deferred {
            frames.append(&mut handle.left);
        }
        if leaves {
            assert!(
                !matches!(asked, Err(OpLogError::CutBack)),
                "{who} tells of a cut for what its sink answers for"
            );
            handle.left.append(&mut frames);
            handle.head_left = Some((handle.log.anchor(), handle.log.appended_len()));
        }
        self.read_anew_since(index, before);
        self.meanwhile_done(unsettled);
        let frames = self.answered_for(index, frames);
        self.trace.push(format!(
            "  asked for {} frames on no ticket, it answers {asked:?}",
            frames.len() + self.handles[index].left.len() * leaves as usize
        ));
        match &asked {
            Ok(()) if !leaves => self.told_synced(&frames, &who),
            Err(OpLogError::CutBack) => {
                self.told_cut(&frames, &who);
                self.counts.own_cut.fetch_add(1, Ordering::Relaxed);
                // the ones that are in the log are owed an answer still
                let kept: Vec<Wrote> = frames.into_iter().filter(|f| self.has(f)).collect();
                self.handles[index].mine = kept;
            }
            Err(_) if !leaves => self.handles[index].mine = frames,
            _ => {}
        }
        if !leaves || asked.is_err() || self.rng.one_in(4) {
            return asked;
        }
        self.sink_asked(index, unsettled)
    }

    /// The sink of handle `index` is asked without a ticket: it answers
    /// for the frames the handle left to it.
    pub fn sink_asked(&mut self, index: usize, unsettled: &[Laid]) -> Result<(), OpLogError> {
        let who = format!("the sink of handle {index}, asked without a ticket,");
        let answered = self.handles[index].sink.as_ref().unwrap().fsync();
        self.meanwhile_done(unsettled);
        let left = std::mem::take(&mut self.handles[index].left);
        let left = self.answered_for(index, left);
        let head = self.handles[index].head_left;
        self.trace.push(format!(
            "  its sink, asked for {} frames on no ticket, answers {answered:?}",
            left.len()
        ));
        match &answered {
            Ok(()) => {
                self.told_synced(&left, &who);
                let synced = &self.counts.left_synced;
                synced.fetch_add(left.len() as u64, Ordering::Relaxed);
            }
            Err(OpLogError::CutBack) => {
                // a frame it was left is gone, or what the handle held
                // when it left them is, and it cannot tell whose
                // frames are in the log: one caller is all it knows
                let begun = |end: u64| self.ledger.begun.is_some_and(|at| at < end);
                let past = head.is_some_and(|(last, end)| !self.ledger.holds(last) || begun(end));
                assert!(
                    !left.is_empty() && (self.gone(&left) || past),
                    "{who} tells of a cut: the log holds the {} frames it was left",
                    left.len()
                );
                // with that it has answered for all of them: asked
                // again, it tells of no cut
                let again = self.handles[index].sink.as_ref().unwrap().fsync();
                let twice = matches!(again, Err(OpLogError::CutBack));
                assert!(!twice, "{who} tells of a cut twice");
                let told = &self.counts.left_told;
                told.fetch_add(left.len() as u64, Ordering::Relaxed);
            }
            Err(_) => self.handles[index].left = left,
        }
        self.by_sink = true;
        answered
    }
}
