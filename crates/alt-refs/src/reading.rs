//! The ref store as a command that only reads holds it: no file of the
//! log changes under it, whatever state a writer left the log in. What is
//! there to cut or to bound is left for the next command that writes.

use std::path::PathBuf;
use std::sync::OnceLock;

use alt_git_codec::ObjectId;
use alt_oplog::OpLogError;

use crate::{AlsoNamed, RefError, RefStore, fold_new};

/// How many ops at the end of the log the stores of this process leave
/// out ([`RefStore::open_confirmed`]).
static LEFT_OUT: OnceLock<usize> = OnceLock::new();

/// How many ops at the end of the log this process answers without: those
/// that were never synced and name objects which are not stored. `None`
/// when it found none.
pub fn left_out() -> Option<usize> {
    LEFT_OUT.get().copied()
}

/// What an open starts from: the refs of `snapshot`, the op of `recent`
/// to replay from, and where the snapshot ends in the log and how long it
/// is. Nothing of a snapshot the log no longer leads to.
pub(crate) fn start_from(
    snapshot: Option<crate::snapshot::Snapshot>,
    anchored: bool,
    recent: &[alt_oplog::Op],
) -> (crate::Refs, usize, u64, u64) {
    match snapshot {
        Some(snap) if anchored => (snap.refs, 0, snap.anchor.map_or(0, |a| a.end), snap.len),
        // the whole log was replayed: the snapshot is only usable if its
        // op is still in the chain
        Some(snap) => match recent.iter().position(|op| op.id == snap.at_op) {
            Some(index) => (snap.refs, index + 1, 0, snap.len),
            None => (crate::Refs::default(), 0, 0, 0),
        },
        None => (crate::Refs::default(), 0, 0, 0),
    }
}

impl RefStore {
    /// [`Self::open_reading`] for a command that answers from the refs:
    /// the log is taken as ending before the first op that was never
    /// synced and names an object `stored` does not vouch for
    /// ([`crate::all_stored`]), where the next command that writes cuts
    /// it. Every later open of the log by this process ends there too.
    pub fn open_confirmed(
        alt_dir: impl Into<PathBuf>,
        also: Option<&AlsoNamed>,
        stored: impl Fn(&ObjectId) -> bool,
    ) -> Result<Self, RefError> {
        let alt_dir = alt_dir.into();
        let store = Self::open_reading(&alt_dir)?;
        let Some(found) = store.unheld(also, stored)? else {
            return Ok(store);
        };
        alt_oplog::end_reads_at(&alt_dir.join("oplog"), found.at)?;
        let _ = LEFT_OUT.set(found.ops);
        Self::open_reading(alt_dir)
    }

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
