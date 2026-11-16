//! The fsync handle a daemon syncs the object database through, off the
//! write path.

use std::fs::{File, OpenOptions};
use std::path::PathBuf;

use super::Odb;
use crate::OdbError;
use crate::head;
use crate::lock::lock_shared;
use crate::synced::{self, Deferred};

/// Off-write-path fsync handle for a [`crate::NativeOdb`]: the blob store (chunks then
/// blobmap) then `map.alt`, the durability order so a crash never leaves a
/// durable identity record pointing at lost content. Holds its own fds, so the
/// daemon fsyncs without `&mut NativeOdb` and appends overlap the fsync.
pub struct OdbSink {
    blobs: alt_store::BlobSink,
    tier1: File,
    map: File,
    deferred: Deferred,
    lock_path: PathBuf,
    durable_path: PathBuf,
}

impl Odb {
    /// An independent fsync handle (chunks → blobmap → tier1 → `map.alt`)
    /// for the daemon's off-write-path group commit.
    pub fn sink(&self) -> Result<OdbSink, OdbError> {
        Ok(OdbSink {
            blobs: self.blobs.sink()?,
            tier1: self.tier1()?.sync_handle()?,
            map: self.map.sync_handle()?,
            deferred: self.deferred.clone(),
            lock_path: self.alt_dir.join("odb.lock"),
            durable_path: self.durable_path.clone(),
        })
    }
}

impl OdbSink {
    /// Fsyncs the files, then records in the marker how far the handle
    /// this sink was made from had written whole batches before the fsync.
    pub fn fsync(&self) -> Result<(), OdbError> {
        let pending = *self.deferred.lock().unwrap();
        self.blobs.fsync()?;
        self.tier1.sync_all()?;
        self.map.sync_all()?;
        let Some((lens, generation)) = pending else {
            return Ok(());
        };
        // a rewind holds the append lock exclusively: it is over or has not
        // begun, and the marker's generation tells which
        let lock = OpenOptions::new().write(true).open(&self.lock_path)?;
        lock_shared(&lock)?;
        if let Some(whole) = synced::record(&self.durable_path, generation, lens, Some(lens))? {
            head::note_synced(&lock, whole.seq, whole.pack);
        }
        Ok(())
    }
}
