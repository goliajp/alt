//! Writing one index run file (the layout is described in [`crate::runs`]).

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use crate::StoreError;
use crate::runs::{ENTRY_LEN, Guard, HEADER_LEN, MAGIC, VERSION, check8};

/// Entries from which a run's fanout table is keyed by two bytes.
const WIDE_FANOUT_FROM: usize = 1 << 16;

/// Streams the bytewise merge of the sorted `sources` into a run file.
pub(crate) fn write_run(
    path: &Path,
    sources: &mut [&[u8]],
    total: usize,
    start: u64,
    end: u64,
    guard: Guard,
    durable: bool,
) -> Result<(), StoreError> {
    if u32::try_from(total).is_err() {
        return Err(StoreError::Format("index run too large"));
    }
    let bits: u8 = if total >= WIDE_FANOUT_FROM { 16 } else { 8 };
    let mut header = [0u8; HEADER_LEN];
    header[..4].copy_from_slice(&MAGIC);
    header[4] = VERSION;
    header[5] = bits;
    header[8..16].copy_from_slice(&(total as u64).to_le_bytes());
    header[16..24].copy_from_slice(&start.to_le_bytes());
    header[24..32].copy_from_slice(&end.to_le_bytes());
    header[32..48].copy_from_slice(&guard);
    let check = check8(&header[..48]);
    header[48..].copy_from_slice(&check);

    let mut out = BufWriter::with_capacity(1 << 20, File::create(path)?);
    let mut hasher = alt_hash::blake3::Hasher::new();
    out.write_all(&header)?;
    let mut buckets = vec![0u32; 1usize << bits];
    loop {
        let mut pick: Option<usize> = None;
        for (i, source) in sources.iter().enumerate() {
            if source.is_empty() {
                continue;
            }
            if pick.is_none_or(|p| source[..ENTRY_LEN] < sources[p][..ENTRY_LEN]) {
                pick = Some(i);
            }
        }
        let Some(pick) = pick else { break };
        let (entry, rest) = sources[pick].split_at(ENTRY_LEN);
        sources[pick] = rest;
        let prefix = if bits == 8 {
            entry[0] as usize
        } else {
            u16::from_be_bytes([entry[0], entry[1]]) as usize
        };
        buckets[prefix] += 1;
        hasher.update(entry);
        out.write_all(entry)?;
    }
    let mut running = 0u32;
    for bucket in &mut buckets {
        running += *bucket;
        *bucket = running;
        let bytes = running.to_le_bytes();
        hasher.update(&bytes);
        out.write_all(&bytes)?;
    }
    out.write_all(&hasher.finalize().as_bytes()[..8])?;
    let file = out.into_inner().map_err(|e| e.into_error())?;
    if durable {
        file.sync_all()?;
    }
    Ok(())
}
