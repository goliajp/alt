//! Taking records out of the middle of a log, with every record after
//! them kept.
//!
//! A log is cut at its end and nowhere else, so a record that has to go
//! from between others is written over with the last record that stays,
//! and the file is cut one record shorter. A record moved this way comes
//! to lie before records it was written after. Where a log holds two
//! records of one key, each names content that is stored, and which of
//! them a lookup finds makes no difference.
//!
//! The change is made on the file by its path, by whoever holds the log's
//! write lock, in two steps a caller keeps apart ([`Removal::fill`], then
//! [`Removal::cut`]) and can make again after a crash, having asked
//! [`Removal::pending`] whether the file still ends in the bytes the
//! removal was planned over.

use std::fs::{File, OpenOptions};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use super::RecLog;
use crate::StoreError;
use crate::pack::read_exact_at;

/// What taking records out of a log changes in its file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Removal {
    /// how long the file is before
    pub was: u64,
    /// how long after
    pub len: u64,
    /// BLAKE3 of the bytes from `len` to `was`
    pub end: [u8; 32],
    /// where a record that stays is written over one that goes, by
    /// position
    pub moves: Vec<(u64, Vec<u8>)>,
}

impl RecLog {
    /// What it takes to remove the records that begin at the bytes
    /// `gone`, in ascending order. Changes nothing. The caller holds the
    /// write lock and has read the log to its end.
    pub fn removal(&self, gone: &[u64]) -> Result<Removal, StoreError> {
        let rec_len = self.layout.rec_len as u64;
        let (mut first, mut last) = (0, gone.len());
        let mut len = self.len;
        let mut moves = Vec::new();
        while first < last {
            if gone[last - 1] == len - rec_len {
                last -= 1;
            } else {
                moves.push((gone[first], self.read(len - rec_len)?.to_vec()));
                first += 1;
            }
            len -= rec_len;
        }
        let mut end = vec![0u8; (self.len - len) as usize];
        read_exact_at(&self.file, &mut end, len)?;
        Ok(Removal {
            was: self.len,
            len,
            end: *alt_hash::blake3::hash(&end).as_bytes(),
            moves,
        })
    }
}

impl Removal {
    /// Whether nothing goes.
    pub fn is_empty(&self) -> bool {
        self.was == self.len
    }

    /// Whether the file at `path` still ends in the records this removal
    /// was planned over: it has not been carried out on it, and nothing
    /// was appended to it or cut off it since.
    pub fn pending(&self, path: &Path) -> Result<bool, StoreError> {
        let file = File::open(path)?;
        if self.is_empty() || file.metadata()?.len() != self.was {
            return Ok(false);
        }
        let mut end = vec![0u8; (self.was - self.len) as usize];
        read_exact_at(&file, &mut end, self.len)?;
        Ok(*alt_hash::blake3::hash(&end).as_bytes() == self.end)
    }

    /// Writes the records that stay over the ones that go and fsyncs
    /// them; the index runs that reach past the first of them go, and
    /// that is fsynced too. The file keeps its length.
    pub fn fill(&self, path: &Path) -> Result<(), StoreError> {
        let file = OpenOptions::new().write(true).open(path)?;
        for (at, record) in &self.moves {
            file.write_all_at(record, *at)?;
        }
        file.sync_all()?;
        let from = self.moves.first().map_or(self.len, |(at, _)| *at);
        let mut idx_dir = path.as_os_str().to_owned();
        idx_dir.push(".idx");
        crate::runs::remove_past(&PathBuf::from(idx_dir), from)?;
        Ok(())
    }

    /// Cuts the file to its new length and fsyncs the cut.
    pub fn cut(&self, path: &Path) -> Result<(), StoreError> {
        let file = OpenOptions::new().write(true).open(path)?;
        file.set_len(self.len)?;
        file.sync_all()?;
        Ok(())
    }
}
