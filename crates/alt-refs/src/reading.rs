//! The ref store as a command that only reads holds it: no file of the
//! log changes under it, whatever state a writer left the log in. What is
//! there to cut or to bound is left for the next command that writes.

use std::path::PathBuf;

use alt_oplog::OpLogError;

use crate::{RefError, RefStore, fold_new};

impl RefStore {
    /// Opens the store to be read and nothing else, for a check that must
    /// leave it as it finds it: the log is not repaired and the snapshot is
    /// not refreshed.
    pub fn open_read_only(alt_dir: impl Into<PathBuf>) -> Result<Self, RefError> {
        Self::open_as(alt_dir.into(), true, false)
    }

    /// Opens the store for a command that only reads: the log is neither
    /// created nor repaired. A snapshot that has fallen behind is written
    /// anew, as by any open: it is a cache, and the next open is short for
    /// it whoever runs it.
    pub fn open_reading(alt_dir: impl Into<PathBuf>) -> Result<Self, RefError> {
        Self::open_as(alt_dir.into(), true, !alt_oplog::reading_past_damage())
    }

    /// Whether this store appended to its log: a drop of it then syncs
    /// the log once more.
    pub fn appended(&self) -> bool {
        self.oplog.appended()
    }

    /// [`Self::refresh`] for a store held across commands, before one that
    /// only reads: the ops other writers committed are folded in under the
    /// log's shared lock and nothing is written. An append that never
    /// finished stays at the end of the log, passed over.
    pub fn follow(&mut self) -> Result<(), RefError> {
        match self.oplog.follow() {
            // another writer cut ops this handle held
            Err(OpLogError::CutBack) => self.oplog.reopen()?,
            followed => followed?,
        }
        if self.oplog.reads() != self.reads_seen {
            return self.rederive();
        }
        fold_new(
            &mut self.refs,
            &mut self.applied,
            &mut self.by_key,
            self.oplog.recent(),
        )?;
        Ok(())
    }
}
