//! The fsync a daemon makes of the log off the write path, through a
//! handle of its own, and how far a log handle knows its appends synced.

use std::fs::File;
use std::os::unix::fs::FileExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::v2::log::Log;
use crate::v2::tail::{Held, Tail};
use crate::{Op, OpLogError, SyncNamed};

/// What a handle's appends reached when it last asked for them to be
/// synced, for its sink to record.
#[derive(Debug, Clone, Default)]
pub struct Reached {
    /// where its last frame ends, zero when there is nothing to sync
    target: u64,
    /// the four bytes of the file before that
    closes: [u8; 4],
    generation: u32,
    /// the ops up to there that nobody recorded as synced
    pending: Vec<Op>,
    /// the furthest boundary the handle or its sink saw recorded
    synced: u64,
}

pub(crate) type Deferred = Arc<Mutex<Reached>>;

impl Log {
    /// Leaves what this handle appended so far for its sink to sync and
    /// to record.
    pub(super) fn defer_sync(&mut self) -> Result<(), OpLogError> {
        let synced = self.tail().map_or(0, |tail| tail.synced);
        let pending = self.named_from(synced.max(self.synced_here()).max(self.synced))?;
        let mut reached = self.deferred.lock().unwrap();
        reached.target = self.state.end;
        reached.closes = self.closes;
        reached.generation = self.state.generation;
        reached.pending = pending;
        Ok(())
    }

    pub(super) fn note_synced(&self, synced: u64) {
        let mut reached = self.deferred.lock().unwrap();
        reached.synced = reached.synced.max(synced);
    }

    pub fn synced_here(&self) -> u64 {
        self.deferred.lock().unwrap().synced
    }
}

pub(crate) struct Sink {
    pub(super) file: File,
    pub(super) dir: PathBuf,
    pub(super) boot: Option<[u8; 16]>,
    pub(super) appended: Deferred,
    pub(super) named: Option<SyncNamed>,
}

impl Sink {
    fn covered(&self, target: u64) -> bool {
        Tail::read(&self.dir, self.boot).is_some_and(|tail| tail.synced >= target)
    }

    pub fn reached(&self) -> Result<Reached, OpLogError> {
        let reached = self.appended.lock().unwrap().clone();
        if reached.target != 0
            && !self.covered(reached.target)
            && let Some(sync) = &self.named
        {
            sync(&reached.pending)?;
        }
        Ok(reached)
    }

    pub fn fsync_to(&self, reached: Reached) -> Result<(), OpLogError> {
        if reached.target == 0 {
            self.file.sync_all()?;
            return Ok(());
        }
        let held = Held::take(&self.dir)?;
        let mut closes = [0u8; 4];
        let mut tail = Tail::read(&self.dir, self.boot).unwrap_or_default();
        let in_place = self.file.metadata()?.len() >= reached.target
            && self
                .file
                .read_exact_at(&mut closes, reached.target - 4)
                .is_ok()
            && closes == reached.closes
            && !tail.cuts(reached.generation, reached.target);
        if !in_place {
            // the frames were cut: there is nothing of this handle's to
            // sync until it appends again
            let mut appended = self.appended.lock().unwrap();
            if appended.target == reached.target {
                (appended.target, appended.pending) = (0, Vec::new());
            }
            return Err(OpLogError::CutBack);
        }
        if tail.synced < reached.target {
            self.file.sync_all()?;
            tail.generation = tail.generation.max(reached.generation);
            tail.raise(reached.target, Some(reached.closes));
            tail.judged = tail.judged.max(reached.target);
            tail.write(&self.dir, self.boot, &held)?;
        }
        let mut appended = self.appended.lock().unwrap();
        appended.synced = appended.synced.max(reached.target);
        Ok(())
    }
}
