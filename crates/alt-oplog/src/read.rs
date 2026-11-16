//! Reading the log file for an open: what has to be read, verifying it,
//! and repairing a torn tail.

use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;

use crate::scan::{
    CHECKSUM_LEN, HEADER_LEN, check_header, ends_the_log, file_header, replay_records,
};
use crate::synced::{self, Marker, State};
use crate::{Anchor, Op, OpLogError, ROOT};

/// Bytes read from the log file under its lock, not yet verified.
pub(crate) struct Raw {
    /// the anchor the read started after, when it still matches the file
    pub base: Option<Anchor>,
    /// file offset of `data[0]`
    at: u64,
    data: Vec<u8>,
    clamped: bool,
}

/// The verified ops of a [`Raw`] read, and where the last whole record ends
/// against how long the file is.
pub(crate) struct Scan {
    pub base: Option<Anchor>,
    pub ops: Vec<Op>,
    pub valid_len: u64,
    pub last_start: u64,
    pub file_len: u64,
    /// no whole record can follow the one the scan stopped at
    pub last: bool,
    /// the read stopped short of the file's end on request
    pub clamped: bool,
}

impl Scan {
    /// A new log without its header yet.
    pub fn headless(&self) -> bool {
        self.valid_len < HEADER_LEN as u64
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
    let meta = file.metadata()?;
    let size = crate::settle::read_end(&meta);
    let clamped = size < meta.len();
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
            clamped,
        });
    }
    Ok(Raw {
        base: None,
        at: 0,
        data: read_at(file, 0, size as usize)?,
        clamped,
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
        last: ends_the_log(&raw.data, replayed.valid),
        clamped: raw.clamped,
    })
}

impl Scan {
    /// The scan as an open takes it: an unfinished append at the end of
    /// the log is passed over, damage is reported, the file stays as it is.
    pub fn unrepaired(mut self, durable: &Path) -> Result<Self, OpLogError> {
        if self.valid_len >= HEADER_LEN as u64 && self.valid_len < self.file_len {
            unfinished(durable, self.valid_len, self.last)?;
        }
        self.file_len = self.valid_len;
        Ok(self)
    }
}

/// A record that does not read at `at`: an append that never finished or
/// damage, as the marker tells (see [`Marker::unfinished`]).
pub(crate) fn unfinished(durable: &Path, at: u64, last: bool) -> Result<(), OpLogError> {
    if Marker::unfinished(durable, at, last) {
        Ok(())
    } else {
        Err(OpLogError::Format("oplog record corrupt"))
    }
}

/// Rescans a log that had no header and gives it one, with its marker;
/// the caller holds the exclusive lock. Another opener may have done so in
/// between: the log is then taken as it is.
pub(crate) fn scan_and_create(
    file: &mut File,
    anchor: Option<Anchor>,
    durable: &Path,
) -> Result<Scan, OpLogError> {
    let mut scan = scan(read_log(file, anchor)?)?;
    if scan.headless() {
        if scan.file_len > 0 {
            synced::cut(durable, file, 0)?;
        }
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&file_header())?;
        file.sync_all()?;
        scan.valid_len = HEADER_LEN as u64;
        scan.file_len = scan.valid_len;
        synced::update(durable, |marker| {
            (*marker == Marker::Absent).then_some(State {
                flags: 0,
                eof: scan.valid_len,
                whole: scan.valid_len,
            })
        })?;
        synced::note_bounded(durable)?;
    }
    Ok(scan)
}
