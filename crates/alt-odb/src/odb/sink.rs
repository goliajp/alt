//! The fsync handle a daemon syncs the object database through, off the
//! write path.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use super::Odb;
use crate::head;
use crate::lock::lock_shared;
use crate::synced::{self, Deferred, Marker};
use crate::{NativeOdb, OdbError};

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
            head::note_synced(&lock, whole);
        }
        Ok(())
    }
}

impl NativeOdb {
    /// [`Self::flush`] for a batch that something other than the
    /// operation log is about to name (a file written with its own fsync):
    /// the batch is synced when this returns, also where fsyncs are
    /// otherwise left to a sink.
    pub fn flush_synced(&mut self) -> Result<(), OdbError> {
        self.flush()?;
        if self.defer && !alt_store::relaxed_durability() {
            self.sink()?.fsync()?;
        }
        Ok(())
    }

    /// Whether the marker records every record this handle holds as
    /// synced. Reads the marker and takes no lock.
    pub fn confirmed(&self) -> Result<bool, OdbError> {
        let odb = self.odb()?;
        let holds = odb.holds()?;
        let marker = Marker::read(&odb.durable_path);
        let recorded = |state: &synced::State| state.whole.reaches(&holds);
        Ok(marker.trusted() && marker.recorded().is_some_and(recorded))
    }

    /// Syncs what the database under `alt_dir` holds, whoever wrote it,
    /// and records that: for a caller about to record as synced something
    /// of another writer that names objects, which that writer stored
    /// first but may not have synced.
    ///
    /// The database is opened the way a reader opens it, which waits for
    /// the shared lock on `odb.lock`: the caller holds no lock that is
    /// taken after that one, the operation log's among them. A rewind or
    /// a cut meanwhile moves the marker's generation, and under another
    /// generation than the one read first nothing is recorded.
    pub fn sync_stored(alt_dir: &Path) -> Result<(), OdbError> {
        let durable_path = alt_dir.join("odb.durable");
        let generation = Marker::read(&durable_path)
            .recorded()
            .map(|state| state.generation);
        let mut this = NativeOdb::open(alt_dir)?;
        let odb = this.odb_mut()?;
        let holds = odb.holds()?;
        odb.blobs.fsync()?;
        odb.tier1_mut()?.sync()?;
        odb.map.fsync()?;
        // a marker without boundaries gets them from a writer
        if let Some(generation) = generation {
            synced::record(&durable_path, generation, holds, Some(holds))?;
        }
        Ok(())
    }
}
