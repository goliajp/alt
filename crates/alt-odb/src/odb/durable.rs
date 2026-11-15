//! Durability of the object database: the group-commit fsync and its
//! marker, write cursors, and rolling a batch back.

use std::fs::OpenOptions;
use std::path::Path;

use super::Odb;
use crate::lock::{lock_exclusive, lock_shared, unlock};
use crate::{OdbCheckpoint, OdbError};

/// The 3 durable EOFs (active pack, blobmap, map.alt) from the marker file;
/// all-zero when missing (everything must be re-fsynced).
fn read_durable(path: &Path) -> [u64; 3] {
    match std::fs::read(path) {
        Ok(b) if b.len() >= 24 => [
            u64::from_le_bytes(b[0..8].try_into().unwrap()),
            u64::from_le_bytes(b[8..16].try_into().unwrap()),
            u64::from_le_bytes(b[16..24].try_into().unwrap()),
        ],
        _ => [0; 3],
    }
}

/// Atomically records the durable EOFs (temp + rename) so a lock-free reader
/// sees the old or new value, never a torn one. Written only after the fsync,
/// so the marker never claims durability that has not happened.
fn write_durable(path: &Path, eofs: [u64; 3]) -> std::io::Result<()> {
    let mut buf = [0u8; 24];
    buf[0..8].copy_from_slice(&eofs[0].to_le_bytes());
    buf[8..16].copy_from_slice(&eofs[1].to_le_bytes());
    buf[16..24].copy_from_slice(&eofs[2].to_le_bytes());
    let tmp = path.with_extension("durable.tmp");
    std::fs::write(&tmp, buf)?;
    std::fs::rename(&tmp, path)
}

fn covers(durable: [u64; 3], target: [u64; 3]) -> bool {
    durable[0] >= target[0] && durable[1] >= target[1] && durable[2] >= target[2]
}

impl Odb {
    /// Finish an unfinished object batch, including after a command error.
    /// Unlike `flush`, an idle call neither synchronizes nor counts a write.
    /// Daemons call this before handing a request to a durability barrier so
    /// a failed put cannot retain its exclusive lock across requests.
    pub fn flush_pending(&mut self) -> Result<(), OdbError> {
        if self.held {
            self.flush()?;
        }
        Ok(())
    }

    /// Durability point: blob store first (chunks before blobmap inside),
    /// then `map.alt`, so a crash never leaves a durable identity record
    /// pointing at lost content.
    pub fn flush(&mut self) -> Result<(), OdbError> {
        // my durability target = how far my appends reached in each file
        let (pack, blobmap) = self.blobs.appended_lens()?;
        let target = [pack, blobmap, self.map.appended_len()];
        // still under the append lock: fold what this batch (and any writer
        // before it) appended into the index runs, so the next open does not
        // have to scan it. The runs are a cache: failing to write them must
        // not keep the batch from its fsync
        let _ = self.index_tails();
        // release the exclusive append lock first, so other writers append (and
        // pile onto the same fsync) instead of waiting behind my durability
        self.release();

        if alt_store::relaxed_durability() {
            return Ok(());
        }
        if self.defer {
            // the daemon's group-commit coordinator fsyncs off the write path
            // (via `sink`); here we only note that a write happened
            self.write_count += 1;
            return Ok(());
        }
        self.sync_to(target)
    }

    pub(super) fn index_tails(&mut self) -> Result<(), OdbError> {
        if !self.held {
            return Ok(());
        }
        self.blobs.index_tail()?;
        if let Some(tier1) = self.tier1.get_mut() {
            tier1.index_tail()?;
        }
        self.map.index_tail()
    }

    /// Snapshots the write cursor across all three append files (chunks pack,
    /// blobmap, map.alt) plus tier1, so a later [`Self::rewind`] can roll back
    /// every object written after it. Acquires the odb write lock + reconciles
    /// in-memory state with disk, captures the cursor, then releases the lock
    /// the same way [`Self::refresh`] does — keeping the file lock symmetric
    /// across request boundaries so a checkpoint at the start of a batch does
    /// not bleed into a long-held flock that blocks other readers/writers.
    pub fn checkpoint(&mut self) -> Result<OdbCheckpoint, OdbError> {
        self.acquire()?;
        let snapshot = self.cursor();
        self.release();
        snapshot
    }

    /// Cheap cursor snapshot of the in-memory write offsets, without touching
    /// the file lock or reconciling against the disk. Safe only when the
    /// caller already holds a write barrier (in-process: the store [`Mutex`];
    /// cross-process: the active flock from a prior [`Self::checkpoint`]) and
    /// just wants a post-batch snapshot to compare against the pre-batch one
    /// — i.e. "did the bytes I just wrote stay put, or did someone else's
    /// commit slip in?". Two extra syscalls saved per receive-pack request,
    /// which is what keeps the group-commit coalescing test still green
    /// with the rewind window in place.
    pub fn cursor(&self) -> Result<OdbCheckpoint, OdbError> {
        Ok(OdbCheckpoint {
            blobs: self.blobs.checkpoint()?,
            tier1_len: self.tier1()?.appended_len(),
            map_len: self.map.appended_len(),
        })
    }

    /// Rolls every odb write since `ckpt` back: truncates each append file to
    /// its captured length, drops the affected in-memory indices, fsyncs the
    /// new lengths, and resets the durable marker so a fresh crash recovery
    /// won't claim a longer durable extent than now exists. Held under the odb
    /// write lock — call it with the same lock that produced `ckpt`. Used by
    /// the server's receive-pack path to undo a pack ingest when the signature
    /// / policy / ref-tx gate downstream rejects the push: no orphan objects
    /// are left in the store.
    pub fn rewind(&mut self, ckpt: OdbCheckpoint) -> Result<(), OdbError> {
        self.blobs.rewind(ckpt.blobs)?;
        self.tier1_mut()?.rewind(ckpt.tier1_len)?;
        self.map.rewind(ckpt.map_len)?;
        // marker reflects what is durable; after truncations + fsyncs, that's
        // exactly the rewound EOFs. Without this, a subsequent flush could
        // covers()-skip its fsync against a stale-larger marker.
        let (pack, blobmap) = self.blobs.appended_lens()?;
        write_durable(&self.durable_path, [pack, blobmap, self.map.appended_len()])?;
        Ok(())
    }

    /// Turns deferred durability on or off (the daemon turns it on so its
    /// group-commit coordinator batches fsyncs in-process; the direct CLI
    /// leaves it off and fsyncs inline).
    pub fn set_defer_durability(&mut self, on: bool) {
        self.defer = on;
    }

    /// A monotonic count of deferred writes — the daemon snapshots it around a
    /// request to tell whether the command wrote.
    pub fn write_count(&self) -> u64 {
        self.write_count
    }

    /// An independent fsync handle (chunks → blobmap → tier1 → `map.alt`)
    /// for the daemon's off-write-path group commit.
    pub fn sink(&self) -> Result<OdbSink, OdbError> {
        Ok(OdbSink {
            blobs: self.blobs.sink()?,
            tier1: self.tier1()?.sync_handle()?,
            map: self.map.sync_handle()?,
        })
    }

    /// The fsync-coalescing core (blob store first — chunks before blobmap —
    /// then `tier1` (so the records are durable before any map.alt entry
    /// references their Tier 1 blob ids), then `map.alt`, so a crash never
    /// leaves a durable identity record pointing at lost content). One
    /// fsync covers my appends and any concurrent ones; the marker then
    /// lets the others skip theirs. The 3-tuple marker stays the same —
    /// tier1 isn't tracked in it because it's a small, append-only file
    /// with cheap fsync and unconditionally syncing it preserves backward
    /// compatibility with stores written before Tier 1 existed.
    fn sync_to(&mut self, target: [u64; 3]) -> Result<(), OdbError> {
        // fast path: another writer already fsynced past my appends
        if covers(read_durable(&self.durable_path), target) {
            return Ok(());
        }
        let sync_lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&self.sync_lock_path)?;
        lock_exclusive(&sync_lock)?;
        let result = (|| -> Result<(), OdbError> {
            if covers(read_durable(&self.durable_path), target) {
                return Ok(());
            }
            // Otherwise a concurrent writer could append a map entry after
            // its content file was synchronized but before map.alt's fsync.
            // The batch released its exclusive lock before entering sync_to.
            lock_shared(&self.lock)?;
            let synced = (|| -> Result<(), OdbError> {
                let (pf, bf) = self.blobs.file_lens()?;
                let eofs = [pf, bf, self.map.file_len()?];
                self.blobs.fsync()?; // chunks then blobmap, in order
                // then tier1 (references blob ids above)
                if let Some(tier1) = self.tier1.get_mut() {
                    tier1.sync()?;
                }
                self.map.fsync()?; // then map.alt (may reference tier1 ids)
                write_durable(&self.durable_path, eofs)?;
                Ok(())
            })();
            let _ = unlock(&self.lock);
            synced
        })();
        let _ = unlock(&sync_lock);
        result
    }
}

/// Off-write-path fsync handle for a [`crate::NativeOdb`]: the blob store (chunks then
/// blobmap) then `map.alt`, the durability order so a crash never leaves a
/// durable identity record pointing at lost content. Holds its own fds, so the
/// daemon fsyncs without `&mut NativeOdb` and appends overlap the fsync.
pub struct OdbSink {
    blobs: alt_store::BlobSink,
    tier1: std::fs::File,
    map: std::fs::File,
}

impl OdbSink {
    pub fn fsync(&self) -> Result<(), OdbError> {
        self.blobs.fsync()?;
        self.tier1.sync_all()?;
        self.map.sync_all()?;
        Ok(())
    }
}
