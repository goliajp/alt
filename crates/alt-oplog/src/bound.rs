//! The boundary a writer gives the log before it appends to one whose
//! marker records none that holds.

use crate::lock::{lock_exclusive, unlock};
use crate::synced::{self, Marker};
use crate::{OpLog, OpLogError, relaxed_durability};

impl OpLog {
    /// Sees to it, as a writer does before it appends, that the marker
    /// records a boundary that holds over the log as it is now: for one
    /// about to note the last operation where a power cut must not take
    /// that operation.
    pub fn bound_now(&mut self) -> Result<(), OpLogError> {
        lock_exclusive(&self.file)?;
        let bounded = self.catch_up_bounded();
        let _ = unlock(&self.file);
        bounded
    }

    /// With the lock held: reads the log to its end and sees to it that
    /// its marker records a boundary that holds ([`Self::bound`]). The
    /// lock is held when this returns.
    ///
    /// A boundary takes in the ops other writers appended, and what those
    /// name has to be synced before it does, with whatever else a writer
    /// that left its appends unsynced wrote. That is done with the lock
    /// given up: whoever syncs it takes locks of its own, and those come
    /// before this one. The boundary is then set where the log ended
    /// before the lock was given up, and ops appended since lie past it.
    pub(crate) fn catch_up_bounded(&mut self) -> Result<(), OpLogError> {
        self.catch_up()?;
        let marker = Marker::read(&self.durable_path);
        let named = self
            .named
            .clone()
            .filter(|_| !relaxed_durability() && !marker.trusted() && !self.is_empty());
        let Some(sync) = named else {
            return self.bound(&marker, self.len_bytes);
        };
        let upto = self.len_bytes;
        // what was appended since the last boundary that held, or all of
        // it where none is recorded
        let held = match marker.recorded() {
            Some(state) => state.whole,
            None => marker.unsynced_from().unwrap_or(0),
        };
        let left = match self.first_unheld(held, |_| Ok::<_, OpLogError>(false))? {
            // the ops this handle read last hold them, unless they began
            // before those
            Some(first) => match self.ops.len().checked_sub(first.ops) {
                Some(from) => self.ops[from..].to_vec(),
                None => self.ops()?[self.len() - first.ops..].to_vec(),
            },
            None => Vec::new(),
        };
        let _ = unlock(&self.file);
        let synced = sync(&left);
        lock_exclusive(&self.file)?;
        synced?;
        self.catch_up()?;
        self.bound(&Marker::read(&self.durable_path), upto)
    }

    /// With the lock held and the log read to its end: a log whose marker
    /// does not record a boundary that holds — an older alt wrote it, the
    /// marker was removed, or appends were left unsynced — gets one at
    /// `upto` before anything is appended; what the ops before `upto`
    /// name is synced. Where fsyncs are skipped the marker is made to say
    /// so instead.
    fn bound(&mut self, marker: &Marker, upto: u64) -> Result<(), OpLogError> {
        synced::lost(marker, self.len_bytes)?;
        self.generation = marker.generation();
        if relaxed_durability() {
            // read to its end under the lock, the log holds what the
            // disk does: what follows is left unsynced in this boot
            let said = marker.recorded().is_some() && !marker.trusted();
            if !said || !crate::left::cached(&self.durable_path) {
                crate::left::note(&self.durable_path)?;
            }
            if !said {
                synced::distrust(&self.durable_path)?;
            }
        } else if !marker.trusted() {
            self.file.sync_all()?;
            synced::establish(&self.durable_path, upto)?;
            if let Some(note) = &self.note {
                note.raise(upto);
            }
        }
        Ok(())
    }
}
