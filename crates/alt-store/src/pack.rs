//! altpack on-disk format: file header, record encoding, and the sequential
//! scan that doubles as crash recovery.

use std::fs::File;
use std::io;
use std::path::Path;

use crate::{ChunkId, StoreError};

pub const MAGIC: [u8; 4] = *b"ALTP";
pub const VERSION: u8 = 1;
/// File header: magic + version byte.
pub const HEADER_LEN: usize = 5;
/// Record header: blake3 id, encoding byte, orig_len, stored_len.
pub const REC_HEADER_LEN: usize = 32 + 1 + 4 + 4;

pub const ENC_RAW: u8 = 0;
pub const ENC_ZSTD: u8 = 1;
/// Reserved for lineage deltas (M2/S9) and prism parts (M3+); part of the
/// frozen format so later milestones need no version bump.
pub const ENC_RESERVED_MAX: u8 = 3;

pub fn file_header() -> [u8; HEADER_LEN] {
    [MAGIC[0], MAGIC[1], MAGIC[2], MAGIC[3], VERSION]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordHeader {
    pub id: ChunkId,
    pub encoding: u8,
    pub orig_len: u32,
    pub stored_len: u32,
}

impl RecordHeader {
    pub fn encode(&self) -> [u8; REC_HEADER_LEN] {
        let mut out = [0u8; REC_HEADER_LEN];
        out[..32].copy_from_slice(&self.id.0);
        out[32] = self.encoding;
        out[33..37].copy_from_slice(&self.orig_len.to_le_bytes());
        out[37..41].copy_from_slice(&self.stored_len.to_le_bytes());
        out
    }

    pub fn parse(buf: &[u8; REC_HEADER_LEN]) -> Self {
        let mut id = [0u8; 32];
        id.copy_from_slice(&buf[..32]);
        Self {
            id: ChunkId(id),
            encoding: buf[32],
            orig_len: u32::from_le_bytes(buf[33..37].try_into().unwrap()),
            stored_len: u32::from_le_bytes(buf[37..41].try_into().unwrap()),
        }
    }
}

pub fn check_file_header(data: &[u8]) -> Result<(), StoreError> {
    if data.len() < HEADER_LEN || data[..4] != MAGIC {
        return Err(StoreError::Format("bad altpack header"));
    }
    if data[4] != VERSION {
        return Err(StoreError::Format("unsupported altpack version"));
    }
    Ok(())
}

/// Walks records sequentially and returns every structurally complete one
/// plus the byte length of that valid prefix. A record cut short by a crash
/// (or any garbage after the last complete record) ends the walk — for the
/// active pack the caller truncates to the valid prefix; for a sealed pack a
/// prefix shorter than the file is corruption.
pub fn scan(data: &[u8]) -> Result<(Vec<(RecordHeader, u64)>, u64), StoreError> {
    check_file_header(data)?;
    let mut entries = Vec::new();
    let mut at = HEADER_LEN;
    while let Some(hdr_bytes) = data.get(at..at + REC_HEADER_LEN) {
        let hdr = RecordHeader::parse(hdr_bytes.try_into().unwrap());
        if hdr.encoding > ENC_RESERVED_MAX {
            break;
        }
        let Some(end) = (at + REC_HEADER_LEN).checked_add(hdr.stored_len as usize) else {
            break;
        };
        if end > data.len() {
            break;
        }
        entries.push((hdr, at as u64));
        at = end;
    }
    Ok((entries, at as u64))
}

/// Positioned read that leaves the file cursor alone (the writer owns it).
#[cfg(unix)]
pub fn read_exact_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<()> {
    use std::os::unix::fs::FileExt;
    file.read_exact_at(buf, offset)
}

#[cfg(windows)]
pub fn read_exact_at(file: &File, mut buf: &mut [u8], mut offset: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        match file.seek_read(buf, offset) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "altpack short read",
                ));
            }
            Ok(n) => {
                offset += n as u64;
                buf = &mut buf[n..];
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Durability of file creation/rename needs the directory entry on disk.
#[cfg(unix)]
pub fn fsync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

#[cfg(not(unix))]
pub fn fsync_dir(_dir: &Path) -> io::Result<()> {
    Ok(())
}
