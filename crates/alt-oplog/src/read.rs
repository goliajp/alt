//! Reading the log file for an open: what has to be read, verifying it,
//! and repairing a torn tail.

use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;

use crate::scan::{CHECKSUM_LEN, HEADER_LEN, check_header, file_header, replay_records};
use crate::synced::{self, Marker, State};
use crate::{Anchor, Op, OpLogError, ROOT};

/// Bytes read from the log file under its lock, not yet verified.
pub(crate) struct Raw {
    /// the anchor the read started after, when it still matches the file
    pub base: Option<Anchor>,
    /// file offset of `data[0]`
    at: u64,
    data: Vec<u8>,
}

/// The verified ops of a [`Raw`] read, and where the last whole record ends
/// against how long the file is.
pub(crate) struct Scan {
    pub base: Option<Anchor>,
    pub ops: Vec<Op>,
    pub valid_len: u64,
    pub last_start: u64,
    pub file_len: u64,
}

impl Scan {
    /// A new log without its header yet, or a torn tail to cut.
    pub fn needs_repair(&self) -> bool {
        self.valid_len < HEADER_LEN as u64 || self.valid_len < self.file_len
    }
}

/// Reads `len` bytes at offset `at` without moving the cursor the writer
/// appends at.
#[cfg(unix)]
pub(crate) fn read_at(file: &File, at: u64, len: usize) -> std::io::Result<Vec<u8>> {
    use std::os::unix::fs::FileExt;
    let mut data = vec![0u8; len];
    file.read_exact_at(&mut data, at)?;
    Ok(data)
}

#[cfg(not(unix))]
pub(crate) fn read_at(file: &File, at: u64, len: usize) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut file = file.try_clone()?;
    let mut data = vec![0u8; len];
    file.seek(SeekFrom::Start(at))?;
    file.read_exact(&mut data)?;
    Ok(data)
}

/// Reads what an open has to verify: everything after `anchor` when the
/// record it names still ends where it says (the checksum closing a record
/// is the op id's first bytes), the whole file otherwise.
pub(crate) fn read_log(file: &File, anchor: Option<Anchor>) -> std::io::Result<Raw> {
    let size = file.metadata()?.len();
    if let Some(anchor) = anchor
        && anchor.end <= size
        && anchor.start + 4 + (CHECKSUM_LEN as u64) <= anchor.end
        && read_at(file, anchor.end - CHECKSUM_LEN as u64, CHECKSUM_LEN)?
            == anchor.op.0[..CHECKSUM_LEN]
    {
        return Ok(Raw {
            base: Some(anchor),
            at: anchor.end,
            data: read_at(file, anchor.end, (size - anchor.end) as usize)?,
        });
    }
    Ok(Raw {
        base: None,
        at: 0,
        data: read_at(file, 0, size as usize)?,
    })
}

/// Verifies and replays a read taken under the lock.
pub(crate) fn scan(raw: Raw) -> Result<Scan, OpLogError> {
    let mut ops = Vec::new();
    let (from, head, last_start) = match raw.base {
        Some(anchor) => (0, anchor.op, anchor.start),
        None => {
            check_header(&raw.data)?;
            (HEADER_LEN.min(raw.data.len()), ROOT, 0)
        }
    };
    // a record that does not read ends the walk; the caller tells an
    // unfinished append from damage
    let replayed = replay_records(&raw.data, from, head, &mut ops)?;
    Ok(Scan {
        base: raw.base,
        ops,
        valid_len: raw.at + replayed.valid as u64,
        last_start: replayed
            .last_start
            .map_or(last_start, |start| raw.at + start as u64),
        file_len: raw.at + raw.data.len() as u64,
    })
}

impl Scan {
    /// The scan of a log that is only read: an unfinished append at its
    /// end is passed over, damage is reported, the file stays as it is.
    pub fn unrepaired(mut self, durable: &Path) -> Result<Self, OpLogError> {
        if self.valid_len >= HEADER_LEN as u64 && self.valid_len < self.file_len {
            unfinished(durable, self.valid_len)?;
        }
        self.file_len = self.valid_len;
        Ok(self)
    }
}

/// A record that does not read at `at`: an append that never finished when
/// the log was not synced that far, damage otherwise.
pub(crate) fn unfinished(durable: &Path, at: u64) -> Result<(), OpLogError> {
    if Marker::unfinished(durable, at) {
        Ok(())
    } else {
        Err(OpLogError::Format("oplog record corrupt"))
    }
}

/// Rescans and repairs; the caller holds the exclusive lock. A new log
/// gets its header and its marker; an unfinished append is cut off; damage
/// is reported and nothing is cut.
pub(crate) fn scan_and_repair(
    file: &mut File,
    anchor: Option<Anchor>,
    durable: &Path,
) -> Result<Scan, OpLogError> {
    let mut scan = scan(read_log(file, anchor)?)?;
    if scan.valid_len < HEADER_LEN as u64 {
        if scan.file_len > 0 {
            synced::cut(durable, file, 0)?;
        }
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&file_header())?;
        file.sync_all()?;
        scan.valid_len = HEADER_LEN as u64;
        synced::update(durable, |marker| {
            (*marker == Marker::Absent).then_some(State {
                flags: 0,
                eof: scan.valid_len,
                whole: scan.valid_len,
            })
        })?;
    } else if scan.valid_len < scan.file_len {
        unfinished(durable, scan.valid_len)?;
        synced::cut(durable, file, scan.valid_len)?;
    }
    scan.file_len = scan.valid_len;
    Ok(scan)
}
