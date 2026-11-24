//! Where the bases that memory has no room for are written, and what all
//! the packs indexed at once may take of the disk there.
//!
//! The directory is the server's own, beside what it stores: where the
//! temporary files of a host are kept in memory, a base written there
//! would have left memory for memory. A base is in it under a name for
//! as long as it takes to write it, and is read through a mapping from
//! then on; what a process that was ended left under a name is removed
//! when the next one opens the directory.
//!
//! A base there is no room for is waited for by a pack that has none in
//! files yet, in the order the packs came, for as long as the pack waits
//! for anything; one that has some makes the base again instead. So no
//! pack waits for the room another that waits has.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use super::memory::{Memory, Taken, Wait};

#[derive(Debug)]
pub struct BaseFiles {
    directory: PathBuf,
    room: Memory,
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
            room: Memory::new(most),
            most_taken: AtomicU64::new(0),
            failed: AtomicU64::new(0),
        })
    }

    pub(super) fn directory(&self) -> &Path {
        &self.directory
    }

    /// Takes `bytes` where they are there to be taken, or, where `wait` is
    /// told, once those before in line have theirs and there is room.
    pub(super) fn take(&self, bytes: u64, wait: Option<Wait<'_>>) -> Option<Taken<'_>> {
        let now = Wait {
            by: Some(std::time::Instant::now()),
            gone: None,
        };
        let taken = self.room.let_in(bytes, 0, wait.unwrap_or(now)).ok()?;
        self.most_taken.fetch_max(self.room.taken().0, Relaxed);
        Some(taken)
    }

    /// room taken as a base in a file takes it, for a test to keep
    #[doc(hidden)]
    pub fn occupy(&self, bytes: u64) -> Option<Taken<'_>> {
        self.take(bytes, None)
    }

    pub(super) fn failed(&self) {
        self.failed.fetch_add(1, Relaxed);
    }

    /// The bytes the bases in files take now, and the most they took at once.
    pub fn taken(&self) -> (u64, u64) {
        (self.room.taken().0, self.most_taken.load(Relaxed))
    }

    /// How many packs wait for room to write a base in.
    pub fn waiting(&self) -> usize {
        self.room.waiting().0
    }

    /// How many bases could not be written, and were made again instead.
    pub fn unwritten(&self) -> u64 {
        self.failed.load(Relaxed)
    }
}
