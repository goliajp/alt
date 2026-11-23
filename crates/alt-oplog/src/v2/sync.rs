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

use crate::lock::{lock_exclusive, unlock};
use crate::v2::log::Log;
use crate::v2::sink::Sink;
use crate::v2::tail::{Held, Tail};
use crate::{OpLogError, SyncNamed, relaxed_durability};

impl Log {
    pub fn sync(&mut self) -> Result<(), OpLogError> {
        // a handle that may not write the log appended nothing to it
        if relaxed_durability() || !self.writable {
            return Ok(());
        }
        if self.defer {
            // the sink fsyncs off the write path and records how far
            self.write_count += 1;
            self.defer_sync();
            return Ok(());
        }
        self.sync_to_end()
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
            appended: self.deferred.clone(),
            named: self.named.clone(),
        })
    }

    pub fn bound_now(&mut self) -> Result<(), OpLogError> {
        lock_exclusive(&self.file)?;
        let caught_up = self.catch_up_bounded();
        let _ = unlock(&self.file);
        caught_up?;
        if relaxed_durability() {
            return Ok(());
        }
        self.sync_to_end()
    }

    /// Records the log as synced up to the last frame this handle holds,
    /// fsyncing it unless another committer's fsync already covered it.
    fn sync_to_end(&mut self) -> Result<(), OpLogError> {
        let target = self.state.end;
        if self.tail().is_some_and(|tail| tail.synced >= target) {
            self.unsynced = false;
            self.note_synced(target);
            return Ok(());
        }
        self.may_write()?;
        if let Some(sync) = &self.named {
            sync(&self.ops_from(self.synced))?;
        }
        let held = Held::take(&self.dir)?;
        // records were cut since the append: not this one's, or there is
        // nothing of it to sync
        if !self.holds_head(self.file.metadata()?.len())? {
            return Err(OpLogError::CutBack);
        }
        let mut tail = Tail::read(&self.dir, self.boot).unwrap_or_default();
        if tail.synced < target {
            self.file.sync_all()?;
            tail.generation = tail.generation.max(self.state.generation);
            tail.raise(target, Some(self.closes));
            tail.judged = tail.judged.max(target);
            tail.write(&self.dir, self.boot, &held)?;
        }
        self.unsynced = false;
        self.synced = self.synced.max(target);
        self.note_synced(target);
        Ok(())
    }
}
