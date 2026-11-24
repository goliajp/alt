//! Making what a handle appended durable, and recording how far.
//!
//! The boundary moves in one way only. A writer takes the end of its own
//! last frame: it read every frame before that under the lock it appended
//! under. What those frames name is synced first, by whoever the caller
//! gave for it, with no lock held. Then, under `lock`, the log is fsynced
//! and `tail` raised to that end and no further: a frame another writer
//! appended meanwhile lies past the boundary, whether or not the fsync
//! took it to the disk, because nobody synced what it names. The next
//! frame anyone appends carries the boundary into the log.

//!
//! No sync says of frames that were cut that they are synced. A handle
//! that is asked to sync and holds frames which are no longer in the log
//! is told so (`CutBack`), and so is one that wrote frames, never had
//! them synced, and read the log anew since without them: once, by the
//! next sync asked of it.

use crate::lock::{lock_exclusive, unlock};
use crate::v2::log::Log;
use crate::v2::sink::{Reached, Sink, record};
use crate::v2::tail::{Held, Tail};
use crate::{OpLogError, SyncNamed, relaxed_durability};

impl Log {
    pub fn sync(&mut self) -> Result<(), OpLogError> {
        // a handle that may not write the log appended nothing to it
        if relaxed_durability() || !self.writable {
            return Ok(());
        }
        let synced = match self.defer {
            // the sink fsyncs off the write path and records how far
            true => self.told_lost().and_then(|()| {
                self.write_count += 1;
                self.defer_sync()
            }),
            false => self.sync_to_end(),
        };
        self.told_cut |= matches!(synced, Err(OpLogError::CutBack));
        synced
    }

    /// Tells, once, that frames this handle wrote and never had synced
    /// were cut off the log.
    fn told_lost(&mut self) -> Result<(), OpLogError> {
        match std::mem::take(&mut self.lost) {
            true => Err(OpLogError::CutBack),
            false => Ok(()),
        }
    }

    pub fn set_defer_durability(&mut self, on: bool) {
        self.defer = on;
    }

    pub fn write_count(&self) -> u64 {
        self.write_count
    }

    pub fn syncs_inline(&self) -> bool {
        !self.defer && !relaxed_durability()
    }

    pub fn sync_named_with(&mut self, sync: SyncNamed) {
        self.named = Some(sync);
    }

    pub fn sync_handle(&self) -> Result<Sink, OpLogError> {
        Ok(Sink {
            file: self.file.try_clone()?,
            dir: self.dir.clone(),
            boot: self.boot,
            id: self.layout.id,
            appended: self.deferred.clone(),
            named: self.named.clone(),
        })
    }

    pub fn bound_now(&mut self) -> Result<(), OpLogError> {
        let bound = self.bound_to_end();
        self.told_cut |= matches!(bound, Err(OpLogError::CutBack));
        bound
    }

    fn bound_to_end(&mut self) -> Result<(), OpLogError> {
        lock_exclusive(&self.file)?;
        let caught_up = self.catch_up_bounded();
        let _ = unlock(&self.file);
        caught_up?;
        if relaxed_durability() {
            return Ok(());
        }
        // over a frame this build would have to know it sets no boundary
        let noted = self.tail();
        if self.said_synced(noted) < self.state.end && !self.knows_all_unlocked()? {
            self.may_append()?;
        }
        self.sync_to_end()
    }

    /// Up to where a sync of this handle records the log as synced, with
    /// the four bytes before that: the last frame it holds. Of a log with
    /// a frame this build would have to know, it is the last frame the
    /// handle wrote itself, while that is not synced: what lies past it
    /// may be named by the frame it does not know.
    pub(super) fn sync_target(&self) -> Result<Option<(u64, [u8; 4])>, OpLogError> {
        match self.knows_all_unlocked()? {
            true => Ok(Some((self.state.end, self.closes))),
            false => Ok(self.unsynced.then_some(self.wrote)),
        }
    }

    /// Records the log as synced up to the last frame this handle holds,
    /// fsyncing it unless another committer's fsync already covered it.
    fn sync_to_end(&mut self) -> Result<(), OpLogError> {
        self.told_lost()?;
        let Some((target, closes)) = self.sync_target()? else {
            return Ok(());
        };
        // what it left for a sink: where it has come to sync by itself
        // since, this sync tells of it in the sink's place
        if self.left_cut(target)? {
            if !self.defer {
                self.left_told();
            }
            return Err(OpLogError::CutBack);
        }
        let noted = self.tail();
        if self.said_synced(noted) >= target {
            self.unsynced = false;
            self.answered_to(target);
            self.left_synced(target);
            return Ok(());
        }
        self.may_write()?;
        if let Some(sync) = &self.named {
            sync(&self.named_unlocked(self.synced)?)?;
        }
        let held = Held::take(&self.dir)?;
        let noted = Tail::noted_held(&self.beside(), &held);
        self.synced = self.synced.max(self.said_synced(noted.says()));
        // frames up to there were cut since the append, or the log tells
        // of a cut that takes them: no boundary is set over them
        let generation = self.state.generation;
        let cut = self.told()?.cuts(generation, self.synced, target);
        if cut || !self.holds_head(self.file.metadata()?.len())? {
            return Err(OpLogError::CutBack);
        }
        record(
            &self.beside(),
            &held,
            noted,
            &Reached::at(target, closes, generation),
        )?;
        self.unsynced = false;
        self.answered_to(target);
        self.synced = self.synced.max(target);
        self.left_synced(target);
        Ok(())
    }
}
