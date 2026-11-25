//! What a handle notes in `tail` of the ops another boot left at the end
//! of the log: that somebody looked at them, or that they are still to
//! be looked at.

use crate::OpLogError;
use crate::v2::log::Log;
use crate::v2::tail::{Held, Tail};

impl Log {
    /// Notes in `tail` what a reader's look came to: every op it holds
    /// vouched for, or the first one that is not at `unheld`, from which
    /// on a file as long as the one it read is no log to a reader. It
    /// holds no lock on the log: of a log that was cut since it read or
    /// is being cut, or that somebody noted or synced further than it
    /// knows, it notes nothing. Nor does one that vouched for all it
    /// holds where the log ended for it short of the file: of the
    /// frames past there it has seen none, and a note that tells of
    /// nothing left would have them taken for looked at.
    pub(super) fn note_look(&self, unheld: Option<u64>) -> std::io::Result<()> {
        if !self.notes()? || (unheld.is_none() && self.hidden.is_some()) {
            return Ok(());
        }
        let (generation, synced) = (self.state.generation, self.synced);
        let (end, seen) = (self.state.end, self.seen);
        // a cut holds this lock from before it tells the log of itself
        let held = Held::take(&self.dir)?;
        if self.told()?.begun(generation) {
            return Ok(());
        }
        Tail::update_held(&self.beside(), &held, |tail| {
            let other = tail.generation != generation && *tail != Tail::default();
            let further = unheld.is_some_and(|at| tail.judged > at || tail.synced > at);
            if other || tail.pending || further {
                return;
            }
            tail.generation = generation;
            // the frames that said so may be among those no longer read
            tail.raise(synced, None);
            match unheld {
                Some(at) => {
                    (tail.pending, tail.judged) = (true, at);
                    (tail.hidden, tail.ends) = (seen, at);
                }
                None => tail.judged = tail.judged.max(end),
            }
        })
    }

    /// Notes in `tail` that every op of the log was vouched for, by a
    /// writer that holds its lock and has read it to its end.
    pub(super) fn note_settled(&self) -> std::io::Result<()> {
        if !self.notes()? {
            return Ok(());
        }
        let (generation, synced, end) = (self.state.generation, self.synced, self.state.end);
        Tail::update(&self.beside(), |tail| {
            tail.stands(generation);
            tail.raise(synced, None);
            (tail.pending, tail.judged) = (false, end);
        })
    }

    /// Whether somebody of this boot has vouched since this handle read
    /// the log for every frame that was in it then: what was appended
    /// after is of this boot, and there is nothing left to look at.
    pub(super) fn vouched_since(&mut self) -> Result<bool, OpLogError> {
        let generation = self.state.generation;
        let vouched = |tail: Tail| !tail.pending && tail.generation == generation;
        let judged = self
            .tail()
            .filter(|tail| vouched(*tail))
            .map(|tail| tail.judged);
        if judged.is_none_or(|judged| judged < self.seen) {
            return Ok(false);
        }
        self.judged.set(u64::MAX);
        Ok(true)
    }

    /// Notes in `tail`, for a writer that appends without a look, that
    /// ops another boot left are still to be looked at: a checkpoint
    /// written after them in this boot says nothing of that.
    pub(super) fn note_left(&mut self) -> Result<(), OpLogError> {
        if self.left_noted {
            return Ok(());
        }
        let (generation, synced) = (self.state.generation, self.synced);
        let judged = self.judged.get();
        Tail::update(&self.beside(), |tail| {
            // what was noted of the log before a cut tells of ops left
            // from where that log was synced
            let noted = tail.pending && tail.generation == generation;
            tail.stands(generation);
            tail.raise(synced, None);
            if !noted {
                (tail.pending, tail.judged) = (true, judged);
            }
        })?;
        self.left_noted = true;
        Ok(())
    }
}
