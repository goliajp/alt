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
//!
//! Each base in a file is a file and a mapping of the process, and a process
//! has so many of either. The bases of all the packs are in no more files at
//! once than [`FILES`], whatever their bytes. A file there is none left of is
//! waited for as room is, by a pack that has no base in files yet; one that
//! has some makes the base again.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::time::Instant;

use super::memory::{Memory, Taken, Wait};
use crate::PackError;

/// the bases all the packs have in files at once
pub const FILES: usize = 4096;

#[derive(Debug)]
pub struct BaseFiles {
    directory: PathBuf,
    room: Memory,
    most_taken: AtomicU64,
    /// the files there may be, one taken for each
    files: Memory,
    most_files: AtomicUsize,
    failed: AtomicU64,
}

/// The room a base has among the files, and that it is one of them: both
/// given back when it is dropped.
#[derive(Debug)]
pub struct Place<'a> {
    _room: Taken<'a>,
    _file: Taken<'a>,
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
            files: Memory::new(FILES as u64),
            most_files: AtomicUsize::new(0),
            failed: AtomicU64::new(0),
        })
    }

    pub(super) fn directory(&self) -> &Path {
        &self.directory
    }

    /// Takes `bytes` where they are there to be taken, or, where `wait` is
    /// told, once those before in line have theirs and there is room. A
    /// file more than there may be is not waited for.
    pub(super) fn take(&self, bytes: u64, wait: Option<Wait<'_>>) -> Option<Place<'_>> {
        let now = Wait {
            by: Some(Instant::now()),
            gone: None,
        };
        let wait = wait.unwrap_or(now);
        let file = self.files.let_in(1, 0, wait).ok()?;
        let taken = self.room.let_in(bytes, 0, wait).ok()?;
        self.most_taken.fetch_max(self.room.taken().0, Relaxed);
        let files = self.files.taken().0 as usize;
        self.most_files.fetch_max(files, Relaxed);
        Some(Place {
            _room: taken,
            _file: file,
        })
    }

    /// Takes `bytes` for a base appended to a thin pack to make it whole,
    /// waiting as [`Self::take`] does. It is in the file of the pack, not in
    /// one of its own: nothing is counted among the files, and a base there
    /// is no room for at all refuses the pack.
    pub(super) fn append(
        &self,
        bytes: u64,
        wait: Option<Wait<'_>>,
    ) -> Result<Taken<'_>, PackError> {
        let now = Wait {
            by: Some(Instant::now()),
            gone: None,
        };
        let taken = self.room.let_in(bytes, 0, wait.unwrap_or(now));
        let taken = taken.map_err(|refused| match refused {
            PackError::Limit(_) => PackError::Limit("index disk"),
            PackError::Busy(_) => PackError::Busy("index disk"),
            other => other,
        })?;
        self.most_taken.fetch_max(self.room.taken().0, Relaxed);
        Ok(taken)
    }

    /// room taken as a base in a file takes it, for a test to keep
    #[doc(hidden)]
    pub fn occupy(&self, bytes: u64) -> Option<Place<'_>> {
        self.take(bytes, None)
    }

    pub(super) fn failed(&self) {
        self.failed.fetch_add(1, Relaxed);
    }

    /// The bytes the bases in files take now, and the most they took at once.
    pub fn taken(&self) -> (u64, u64) {
        (self.room.taken().0, self.most_taken.load(Relaxed))
    }

    /// The bases in files now, and the most there were at once.
    pub fn files(&self) -> (usize, usize) {
        (self.files.taken().0 as usize, self.most_files.load(Relaxed))
    }

    /// How many packs wait for room to write a base in, or for a file.
    pub fn waiting(&self) -> usize {
        self.room.waiting().0 + self.files.waiting().0
    }

    /// How many bases could not be written, and were made again instead.
    pub fn unwritten(&self) -> u64 {
        self.failed.load(Relaxed)
    }
}
