//! Where the bases that memory has no room for are written, and what all
//! the packs indexed at once may take of the disk there.
//!
//! The directory is the server's own, beside what it stores: where the
//! temporary files of a host are kept in memory, a base written there
//! would have left memory for memory. A base is in it under a name for
//! as long as it takes to write it, and is read through a mapping from
//! then on; what a process that was ended left under a name is removed
//! when the next one opens the directory.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

#[derive(Debug)]
pub struct BaseFiles {
    directory: PathBuf,
    most: u64,
    taken: AtomicU64,
    most_taken: AtomicU64,
    failed: AtomicU64,
}

impl BaseFiles {
    /// The files of bases in `directory`, `most` bytes of them at once. The
    /// directory is made where it is not there, is its owner's alone, and
    /// is emptied.
    pub fn open(directory: &Path, most: u64) -> io::Result<Self> {
        alt_os::temp::emptied(directory)?;
        Ok(BaseFiles {
            directory: directory.to_owned(),
            most,
            taken: AtomicU64::new(0),
            most_taken: AtomicU64::new(0),
            failed: AtomicU64::new(0),
        })
    }

    pub(super) fn directory(&self) -> &Path {
        &self.directory
    }

    /// Takes `bytes` where they are there to be taken.
    pub(super) fn take(&self, bytes: u64) -> bool {
        let room = |taken: u64| taken.checked_add(bytes).filter(|all| *all <= self.most);
        let Ok(before) = self.taken.try_update(Relaxed, Relaxed, room) else {
            return false;
        };
        self.most_taken.fetch_max(before + bytes, Relaxed);
        true
    }

    pub(super) fn give_back(&self, bytes: u64) {
        self.taken.fetch_sub(bytes, Relaxed);
    }

    pub(super) fn failed(&self) {
        self.failed.fetch_add(1, Relaxed);
    }

    /// The bytes the bases in files take now, and the most they took at once.
    pub fn taken(&self) -> (u64, u64) {
        (self.taken.load(Relaxed), self.most_taken.load(Relaxed))
    }

    /// How many bases could not be written, and were made again instead.
    pub fn unwritten(&self) -> u64 {
        self.failed.load(Relaxed)
    }
}
