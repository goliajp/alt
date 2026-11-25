//! The cut a log tells of in its own header.
//!
//! ```text
//! [44..48) the generation the log is of once the cut last begun is
//!          made; zero before any
//! [48..56) where that cut is made
//! ```
//!
//! A cut writes this first, under the exclusive lock of the log and
//! `lock`, before it lays its checkpoint over the frames it takes away.
//! From then on the frames past that place are no log, and the file
//! says so itself: to a handle that holds some of them, which is told
//! that they were cut before it appends after them or records them as
//! synced, and to whoever opens the file and would begin at a checkpoint
//! among them. `tail`, beside the log, has no part in this: it may be
//! gone, or of a system that tells no boot and writes none.
//!
//! A log of a generation below the one told is one the cut has not come
//! to, as far as it was read. Read through the place of the cut it
//! either holds the checkpoint of the cut, and is of that generation
//! from there on, or the cut never laid one: its process died first, the
//! frames are all there, and the next writer that reads the log to its
//! end under its lock takes back what the header tells. Until then the
//! log ends at that place for whoever reads without cutting.
//!
//! These bytes are under no checksum: the header's does not cover them.
//! A cut syncs them before it writes its checkpoint, so that a part of
//! the checkpoint is never on disk without them: after a power cut,
//! what does not read at the place they name may be what a cut of ops
//! laid there. A writer that takes them back does not sync that, and a
//! power cut may bring them back over a log the cut never came to,
//! which the next writer takes back again. A place they name that is
//! not one a cut could be made at, before where the log is synced,
//! tells nothing.

use std::fs::File;
use std::os::unix::fs::FileExt;

use crate::OpLogError;
use crate::read::read_at;
use crate::v2::block::{FILE_HEADER_LEN, FileHeader};
use crate::v2::log::Log;

const AT: usize = 44;
const LEN: usize = 12;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Mark {
    pub generation: u32,
    pub at: u64,
}

impl Mark {
    fn of(bytes: &[u8; LEN]) -> Mark {
        Mark {
            generation: u32::from_le_bytes(bytes[..4].try_into().unwrap()),
            at: u64::from_le_bytes(bytes[4..].try_into().unwrap()),
        }
    }

    /// What the file header `header` tells.
    pub fn told(header: &[u8; FILE_HEADER_LEN]) -> Mark {
        Mark::of(header[AT..AT + LEN].try_into().unwrap())
    }

    /// What `file` tells to whoever holds a lock a cut holds: the lock
    /// of the log, shared or not, or `lock` beside it.
    pub fn read(file: &File) -> std::io::Result<Mark> {
        let mut bytes = [0u8; LEN];
        file.read_exact_at(&mut bytes, AT as u64)?;
        Ok(Mark::of(&bytes))
    }

    /// What `file` tells to whoever holds no lock: a read that may have
    /// met the write of a cut halfway is made again.
    pub fn read_unlocked(file: &File) -> std::io::Result<Mark> {
        let mut read = Mark::read(file)?;
        loop {
            let again = Mark::read(file)?;
            if again == read {
                return Ok(read);
            }
            read = again;
        }
    }

    pub fn write(&self, file: &File) -> std::io::Result<()> {
        let mut bytes = [0u8; LEN];
        bytes[..4].copy_from_slice(&self.generation.to_le_bytes());
        bytes[4..].copy_from_slice(&self.at.to_le_bytes());
        file.write_all_at(&bytes, AT as u64)
    }

    /// Whether the cut is none a log of `generation` holds, as far as it
    /// was read.
    pub fn begun(&self, generation: u32) -> bool {
        self.generation > generation
    }

    /// Where the cut is made, when a log of `generation` that is synced
    /// up to `synced` does not hold it as far as it was read: nothing is
    /// cut before where a log is synced.
    pub fn begun_at(&self, generation: u32, synced: u64) -> Option<u64> {
        let from = synced.max(FILE_HEADER_LEN as u64);
        (self.begun(generation) && self.at >= from).then_some(self.at)
    }

    /// Whether a handle that holds the log of `generation` up to `end`,
    /// and knows it synced up to `synced`, holds frames the cut takes
    /// away.
    pub fn cuts(&self, generation: u32, synced: u64, end: u64) -> bool {
        self.begun_at(generation, synced).is_some_and(|at| at < end)
    }
}

/// The header of the log `file` and what it tells of a cut, in one read.
pub(crate) fn header_of(file: &File) -> Result<(FileHeader, Mark), OpLogError> {
    let bytes = read_at(file, 0, FILE_HEADER_LEN)?;
    let header = FileHeader::decode(&bytes)?;
    let told = bytes.first_chunk().map(Mark::told);
    Ok((header, told.unwrap_or_default()))
}

impl Log {
    /// What the header of the log tells of a cut, to a handle that holds
    /// a lock a cut holds.
    pub(super) fn told(&self) -> std::io::Result<Mark> {
        let (bytes, reads) = self.read.get();
        self.read.set((bytes + LEN as u64, reads + 1));
        Mark::read(&self.file)
    }
}
