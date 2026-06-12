//! altidx: the sealed-pack index file. Always a cache, never the truth —
//! it can be rebuilt by scanning the pack, and reads re-hash payloads, so a
//! stale idx surfaces as a corrupt read rather than silent wrong data.

use std::fs::{self, File};
use std::io::Write;
use std::path::Path;

use crate::{ChunkId, StoreError};

pub const MAGIC: [u8; 4] = *b"ALTI";
pub const VERSION: u8 = 1;
/// File header: magic + version + entry count u64.
const HEADER_LEN: usize = 13;
/// Entry: blake3 id + record offset u64.
const ENTRY_LEN: usize = 40;

/// Writes the idx atomically (tmp + fsync + rename) so a crash mid-seal
/// never leaves a half-written idx that looks authoritative.
pub fn write(path: &Path, entries: &[(ChunkId, u64)]) -> Result<(), StoreError> {
    let mut sorted = entries.to_vec();
    sorted.sort_unstable_by_key(|entry| entry.0.0);

    let mut buf = Vec::with_capacity(HEADER_LEN + ENTRY_LEN * sorted.len());
    buf.extend_from_slice(&MAGIC);
    buf.push(VERSION);
    buf.extend_from_slice(&(sorted.len() as u64).to_le_bytes());
    for (id, offset) in &sorted {
        buf.extend_from_slice(&id.0);
        buf.extend_from_slice(&offset.to_le_bytes());
    }

    let tmp = path.with_extension("altidx.tmp");
    let mut file = File::create(&tmp)?;
    file.write_all(&buf)?;
    file.sync_all()?;
    fs::rename(&tmp, path)?;
    Ok(())
}

pub fn read(path: &Path) -> Result<Vec<(ChunkId, u64)>, StoreError> {
    let data = fs::read(path)?;
    if data.len() < HEADER_LEN || data[..4] != MAGIC {
        return Err(StoreError::Format("bad altidx header"));
    }
    if data[4] != VERSION {
        return Err(StoreError::Format("unsupported altidx version"));
    }
    let count = u64::from_le_bytes(data[5..13].try_into().unwrap()) as usize;
    if count
        .checked_mul(ENTRY_LEN)
        .and_then(|n| n.checked_add(HEADER_LEN))
        != Some(data.len())
    {
        return Err(StoreError::Format("altidx length mismatch"));
    }
    let mut entries = Vec::with_capacity(count);
    for chunk in data[HEADER_LEN..].chunks_exact(ENTRY_LEN) {
        let mut id = [0u8; 32];
        id.copy_from_slice(&chunk[..32]);
        let offset = u64::from_le_bytes(chunk[32..].try_into().unwrap());
        entries.push((ChunkId(id), offset));
    }
    Ok(entries)
}
