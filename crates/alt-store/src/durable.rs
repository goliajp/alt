//! Making what a chunk store appended durable.

use std::fs::OpenOptions;
use std::io::{Seek, SeekFrom};
use std::path::PathBuf;

use crate::packs::{list_pack_seqs, pack_path};
use crate::{ChunkStore, StoreError, relaxed_durability};

/// An off-write-path fsync handle for the chunk store's active pack (see
/// [`ChunkStore::sink`]). Holds only the pack directory; it re-opens the
/// highest-seq (active) pack on each `fsync`, so a roll between calls is handled
/// by construction.
pub struct ChunkSink {
    dir: PathBuf,
}

impl ChunkSink {
    /// Fsyncs the active pack. A read+write handle (we only fsync, never write)
    /// is opened fresh so this needs no shared state with the live `ChunkStore`.
    pub fn fsync(&self) -> Result<(), StoreError> {
        if let Some(seq) = list_pack_seqs(&self.dir)?.into_iter().max() {
            let f = OpenOptions::new()
                .read(true)
                .write(true)
                .open(pack_path(&self.dir, seq))?;
            f.sync_all()?;
        }
        Ok(())
    }
}

impl ChunkStore {
    /// Fsyncs the active pack — the durability point between seals.
    pub fn flush(&mut self) -> Result<(), StoreError> {
        if !relaxed_durability() {
            self.fsync()?;
        }
        Ok(())
    }

    /// Raw fsync of the active pack (no relaxed-durability gate) — the group
    /// commit layer decides whether/when to call it.
    pub fn fsync(&mut self) -> Result<(), StoreError> {
        self.active.write.sync_all()?;
        self.unsynced = false;
        Ok(())
    }

    /// Chops what an append that failed left behind, so that the next one
    /// starts at a clean offset. The cut is a change to the file like an
    /// append is: a drop syncs it.
    pub(crate) fn cut_torn(&mut self) -> Result<(), StoreError> {
        self.unsynced = true;
        self.tail.cut(self.active.seq, self.active.len)?;
        self.active.write.set_len(self.active.len)?;
        self.active.write.seek(SeekFrom::Start(self.active.len))?;
        Ok(())
    }

    /// An independent fsync handle (the daemon's group commit fsyncs off the
    /// write path, without `&mut self`, so appends overlap the fsync). It
    /// re-finds the active pack on each call because the pack rolls; sealed
    /// packs are fsynced at seal time, so only the active one needs flushing.
    pub fn sink(&self) -> ChunkSink {
        ChunkSink {
            dir: self.dir.clone(),
        }
    }
}

impl Drop for ChunkStore {
    fn drop(&mut self) {
        // best-effort durability; explicit flush() is the checked path
        if self.unsynced && !relaxed_durability() {
            let _ = self.active.write.sync_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::ChunkStore;

    #[test]
    fn a_drop_owes_an_fsync_only_for_what_was_appended_since_the_last_one() {
        let dir = alt_os::temp::tempdir().unwrap();
        let mut store = ChunkStore::open(dir.path()).unwrap();
        assert!(!store.unsynced);
        store.put(b"first").unwrap();
        assert!(store.unsynced);
        store.fsync().unwrap();
        assert!(!store.unsynced);
        store.put(b"second").unwrap();
        assert!(store.unsynced);
        store.flush().unwrap();
        assert!(!store.unsynced);
        // nothing is appended for a chunk the store already holds
        store.put(b"second").unwrap();
        assert!(!store.unsynced);
    }

    #[test]
    fn an_append_that_fails_after_an_fsync_leaves_a_drop_owing_one() {
        let dir = alt_os::temp::tempdir().unwrap();
        let mut store = ChunkStore::open(dir.path()).unwrap();
        store.put(b"first").unwrap();
        store.fsync().unwrap();
        assert!(!store.unsynced);
        // a handle that cannot write: the append fails
        let pack = crate::packs::pack_path(dir.path(), store.active.seq);
        store.active.write = std::fs::File::open(pack).unwrap();
        assert!(store.put(b"second").is_err());
        assert!(store.unsynced);
    }
}
