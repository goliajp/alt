//! What lies past the last record a reader holds of an append file, for
//! the full check.
//!
//! A reader passes over an append that never finished at the end of a
//! file. What it passes over should hold no record: when a whole record
//! that verifies is found there, something other than an unfinished append
//! stopped the reader short of it, and passing over it would lose it.

use std::path::PathBuf;

use crate::pack::{self, ENC_RESERVED_MAX, REC_HEADER_LEN, RecordHeader};
use crate::reclog::RecLog;
use crate::{BlobStore, ChunkStore, StoreError};

/// How an append file goes on past the records held of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Passed {
    /// where the records held end
    pub holds: u64,
    /// the length of the file
    pub len: u64,
    /// where, past `holds`, a whole record begins that verifies
    pub whole_at: Option<u64>,
}

impl RecLog {
    /// What follows the records held. Records are of one length: each
    /// place a record would begin, counted from where the held ones end,
    /// is tried.
    pub fn passed(&self) -> Result<Passed, StoreError> {
        let (holds, len) = (self.appended_len(), self.file_len()?);
        let mut rest = vec![0u8; len.saturating_sub(holds) as usize];
        pack::read_exact_at(self.file(), &mut rest, holds)?;
        let rec_len = self.rec_len();
        let whole = rest
            .chunks_exact(rec_len)
            .position(|rec| self.intact(rec))
            .map(|n| holds + (n * rec_len) as u64);
        Ok(Passed {
            holds,
            len,
            whole_at: whole,
        })
    }
}

impl ChunkStore {
    /// What follows the records held of the active pack, with the path of
    /// the pack. Records vary in length, so one may begin at any byte:
    /// every byte is tried as the start of a header, and where the header
    /// could be one, its payload is decoded and hashed. That is the cost
    /// of one decode for each such byte, in a part of the file that is
    /// empty unless a write was interrupted.
    pub fn passed(&self) -> Result<(PathBuf, Passed), StoreError> {
        let (holds, len) = (self.checkpoint().pack_len, self.pack_file_len()?);
        let mut rest = vec![0u8; len.saturating_sub(holds) as usize];
        pack::read_exact_at(self.active_file(), &mut rest, holds)?;
        let whole = (0..rest.len().saturating_sub(REC_HEADER_LEN)).find(|&at| {
            let header: &[u8; REC_HEADER_LEN] = rest[at..at + REC_HEADER_LEN].try_into().unwrap();
            let header = RecordHeader::parse(header);
            let payload = at + REC_HEADER_LEN;
            header.encoding < ENC_RESERVED_MAX
                && rest
                    .get(payload..payload + header.stored_len as usize)
                    .is_some_and(|payload| self.check_record(&header, payload).is_ok())
        });
        let passed = Passed {
            holds,
            len,
            whole_at: whole.map(|at| holds + at as u64),
        };
        Ok((self.active_path(), passed))
    }
}

impl BlobStore {
    /// What follows the records held of the active pack and of the blob
    /// map, each with its file.
    pub fn passed(&self) -> Result<[(PathBuf, Passed); 2], StoreError> {
        let map = self.map()?.passed()?;
        Ok([self.chunk_store().passed()?, (self.map_path(), map)])
    }
}
