//! The fsync of the object database's files that a batch ends with, and
//! the handle a daemon syncs them through, off the write path.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use super::Odb;
use crate::head;
use crate::lock::lock_shared;
use crate::synced::{self, Deferred, Lens, Log, Marker};
use crate::tier1::Tier1Map;
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
    ///
    /// A file that an fsync since the last rewind already found as long as
    /// that handle wrote it holds nothing of the handle's that is not
    /// synced, and is left alone.
    pub fn fsync(&self) -> Result<(), OdbError> {
        let Some((lens, generation)) = *self.deferred.lock().unwrap() else {
            self.blobs.fsync()?;
            self.tier1.sync_all()?;
            return Ok(self.map.sync_all()?);
        };
        let marker = Marker::read(&self.durable_path);
        let synced = marker
            .recorded()
            .filter(|state| state.generation == generation);
        let synced = synced.map(|state| state.eof);
        let [pack, blobmap, tier1, map] = [Log::Pack, Log::Blobmap, Log::Tier1, Log::Map]
            .map(|log| grown(synced.as_ref(), &lens, log));
        self.blobs.fsync_of(pack, blobmap)?;
        if tier1 {
            self.tier1.sync_all()?;
        }
        if map {
            self.map.sync_all()?;
        }
        // a rewind holds the append lock exclusively: it is over or has not
        // begun, and the marker's generation tells which
        let lock = OpenOptions::new().write(true).open(&self.lock_path)?;
        lock_shared(&lock)?;
        let eof = (pack || blobmap || tier1 || map).then_some(lens);
        if let Some(whole) = synced::record(&self.durable_path, generation, lens, eof)? {
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

    /// [`Self::confirmed`] for a handle that may not have opened the
    /// database: one that has not holds no record of it.
    pub fn held_confirmed(&self) -> Result<bool, OdbError> {
        match self.odb.get() {
            Some(_) => self.confirmed(),
            None => Ok(true),
        }
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

impl Odb {
    /// Under the sync lock and the shared append lock (so no rewind and no
    /// writer's cut is under way): fsyncs unless one since the last rewind
    /// covered the files as far as `target`, and records it. A file that is
    /// no longer than such an fsync found it holds nothing unsynced, of any
    /// writer, and is left alone.
    pub(super) fn sync_locked(&mut self, target: Lens, generation: u64) -> Result<(), OdbError> {
        let marker = Marker::read(&self.durable_path);
        let synced = marker
            .recorded()
            .filter(|state| state.generation == generation);
        let synced = synced.map(|state| state.eof);
        let eof = if synced.is_some_and(|eof| eof.reaches(&target)) {
            None
        } else {
            let (pack, blobmap) = self.blobs.file_lens()?;
            let eof = Lens {
                seq: target.seq,
                pack,
                blobmap,
                // not opened, not fsynced
                tier1: self.tier1.get().map_or(Ok(0), Tier1Map::file_len)?,
                map: self.map.file_len()?,
            };
            let grown = |log| grown(synced.as_ref(), &eof, log);
            // chunks then blobmap, in order
            self.blobs.fsync_of(grown(Log::Pack), grown(Log::Blobmap))?;
            // then tier1 (references blob ids above)
            if let Some(tier1) = self.tier1.get_mut()
                && grown(Log::Tier1)
            {
                tier1.sync()?;
            }
            // then map.alt (may reference tier1 ids)
            if grown(Log::Map) {
                self.map.fsync()?;
            }
            Some(eof)
        };
        if let Some(whole) = synced::record(&self.durable_path, generation, target, eof)? {
            head::note_synced(&self.lock, whole);
        }
        Ok(())
    }
}

/// Whether `log`, as long as `eof` has it, reaches past what the last
/// fsync recorded covered: `synced` has the lengths that fsync found, and
/// is `None` where no fsync since the last rewind recorded any.
fn grown(synced: Option<&Lens>, eof: &Lens, log: Log) -> bool {
    synced.and_then(|was| was.of(log, eof.seq)) < eof.of(log, eof.seq)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOGS: [Log; 4] = [Log::Pack, Log::Blobmap, Log::Tier1, Log::Map];

    fn lens(seq: u32, pack: u64, blobmap: u64, tier1: u64, map: u64) -> Lens {
        Lens {
            seq,
            pack,
            blobmap,
            tier1,
            map,
        }
    }

    #[test]
    fn a_file_is_synced_again_exactly_when_it_holds_more_than_an_fsync_covered() {
        let was = lens(2, 100, 200, 300, 400);
        let which = |synced: Option<&Lens>, eof: Lens| LOGS.map(|log| grown(synced, &eof, log));
        // nothing recorded: every file
        assert_eq!(which(None, was), [true; 4]);
        // as long as they were: none
        assert_eq!(which(Some(&was), was), [false; 4]);
        // only the files that grew
        let now = lens(2, 141, 200, 300, 482);
        assert_eq!(which(Some(&was), now), [true, false, false, true]);
        // a pack begun since was synced with its header and no further;
        // of an earlier one nothing is known
        let rolled = lens(3, 60, 200, 300, 400);
        assert_eq!(which(Some(&was), rolled), [true, false, false, false]);
        let earlier = lens(1, 100, 200, 300, 400);
        assert_eq!(which(Some(&was), earlier), [true, false, false, false]);
    }
}
