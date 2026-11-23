//! The fsync a daemon makes of the log off the write path, through a
//! handle of its own, and how far a log handle knows its appends synced.

use std::fs::File;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::OpLogError;
use crate::left::open_sync_lock;
use crate::lock::{lock_exclusive, unlock};
use crate::read::read_at;
use crate::scan::CHECKSUM_LEN;
use crate::synced::{self, Marker, SyncNamed};
use crate::v1::OpLog;

/// What a handle's appends reached when it last asked for them to be
/// synced, for its sink to record, and how far the handle and its sink
/// have recorded the log as synced.
#[derive(Debug, Clone, Copy, Default)]
pub struct Reached {
    /// the marker's generation the handle appended under
    generation: u32,
    /// where the last record ends that the handle read and did not write
    others_to: u64,
    /// where its last record ends, zero before it appended any
    target: u64,
    /// the checksum that closes that record
    head: [u8; CHECKSUM_LEN],
    /// the furthest boundary the handle or its sink saw recorded
    synced: u64,
}

pub(crate) type Deferred = Arc<Mutex<Reached>>;

impl OpLog {
    /// Leaves what this handle appended so far for its sink to sync and
    /// to record.
    pub(crate) fn defer_sync(&mut self) {
        let mut reached = self.deferred.lock().unwrap();
        reached.generation = self.generation;
        reached.others_to = self.others_to;
        reached.target = self.len_bytes;
        reached.head = self.head().map_or([0; CHECKSUM_LEN], |head| {
            head.0[..CHECKSUM_LEN].try_into().unwrap()
        });
    }

    /// Notes that the marker records the log as synced up to `whole`.
    pub(crate) fn note_synced(&self, whole: u64) {
        let mut reached = self.deferred.lock().unwrap();
        reached.synced = reached.synced.max(whole);
    }

    /// The furthest boundary this handle, or a sink made from it, saw the
    /// marker record: zero before either synced anything. Nothing is read.
    pub fn synced_here(&self) -> u64 {
        self.deferred.lock().unwrap().synced
    }
}

/// An fsync handle for the log that works without the [`crate::OpLog`] it
/// came from (the daemon fsyncs off the write path): after the fsync it
/// records how far that log's deferred appends had reached before it.
pub struct OpLogSink {
    pub(crate) file: File,
    pub(crate) appended: Deferred,
    pub(crate) durable_path: PathBuf,
    pub(crate) named: Option<SyncNamed>,
    pub(crate) note: Option<crate::NotePlace>,
}

impl OpLogSink {
    /// The first half of [`Self::fsync`]: what the handle's appends
    /// reached, with what the ops of other writers before them name
    /// synced where the marker does not record those ops yet. That takes
    /// locks of its own (see [`OpLog::sync_named_with`]): the caller
    /// holds none that comes after them, and takes what it needs for the
    /// fsync afterwards.
    pub fn reached(&self) -> Result<Reached, OpLogError> {
        let reached = *self.appended.lock().unwrap();
        if reached.target != 0 {
            let marker = Marker::read(&self.durable_path);
            synced::sync_named(&self.named, &marker, reached.others_to)?;
        }
        Ok(reached)
    }

    /// The second half: fsyncs the log and records it as synced up to
    /// where `reached` says the handle's appends ended.
    pub fn fsync_to(&self, reached: Reached) -> Result<(), OpLogError> {
        self.file.sync_all()?;
        if reached.target == 0 {
            return Ok(());
        }
        // a boundary this log wrote, not a file length: only a cut of
        // whole records brings it down, and that changes the generation
        let target = reached.target;
        let recorded = synced::record(&self.durable_path, reached.generation, target, Some(target));
        let whole = match recorded? {
            Some(whole) => whole,
            None => self.record_anew(&reached)?,
        };
        let mut appended = self.appended.lock().unwrap();
        appended.synced = appended.synced.max(whole);
        drop(appended);
        if let Some(note) = &self.note {
            note.raise(whole);
        }
        Ok(())
    }

    pub fn fsync(&self) -> Result<(), OpLogError> {
        self.fsync_to(self.reached()?)
    }

    /// Records were cut since the handle appended, as the marker's
    /// generation tells. Where the record `reached` ends with is still
    /// in its place, they were not the handle's: it is recorded under the
    /// generation the marker is at now, which the handle goes on under.
    /// Where it is gone there is nothing of the handle's to sync
    /// ([`OpLogError::CutBack`]). A cut waits for the sync lock held here.
    fn record_anew(&self, reached: &Reached) -> Result<u64, OpLogError> {
        let sync_lock = open_sync_lock(&self.durable_path.with_file_name("sync.lock"))?;
        lock_exclusive(&sync_lock)?;
        let recorded = (|| {
            let generation = Marker::read(&self.durable_path).generation();
            let target = reached.target;
            let held = self.file.metadata()?.len() >= target
                && read_at(&self.file, target - CHECKSUM_LEN as u64, CHECKSUM_LEN)? == reached.head;
            if !held {
                return Err(OpLogError::CutBack);
            }
            let mut appended = self.appended.lock().unwrap();
            if appended.generation == reached.generation {
                appended.generation = generation;
            }
            drop(appended);
            let whole = synced::record(&self.durable_path, generation, target, Some(target))?;
            whole.ok_or(OpLogError::CutBack)
        })();
        let _ = unlock(&sync_lock);
        recorded
    }
}
