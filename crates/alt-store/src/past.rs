//! Reading a store past damage, to get out what is left of it.
//!
//! A record that does not read and is not an append that never finished
//! is damage, and an open that meets it fails: every command then fails
//! with it, whatever it was after. A process that only reads can ask to go
//! on instead, by `ALT_READ_PAST_DAMAGE` in its environment. An open that
//! does not hold the writers' lock then passes over the damaged bytes,
//! holds the records that read on either side of them, and notes what it
//! passed over ([`passed_damage`]). No file changes. What the damaged
//! bytes held is not there for this process: an object stored in them
//! reads as missing.
//!
//! Nothing may be written by a process that reads this way: what it holds
//! of a log is not the log, and a record log read past damage holds
//! fewer bytes than it read.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use alt_os::map::Mmap;

use crate::pack::{self, ENC_RAW, ENC_ZSTD, REC_HEADER_LEN, RecordHeader};
use crate::{ChunkStore, StoreError};

/// Bytes of a file that an open passed over as damaged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PassedDamage {
    pub file: PathBuf,
    pub from: u64,
    pub to: u64,
}

static PASSED: Mutex<Vec<PassedDamage>> = Mutex::new(Vec::new());

/// Whether this process reads past damage. The environment is asked each
/// time, which is once or twice a process: an answer kept in a static
/// would be a page more for every process to write.
pub fn reading_past_damage() -> bool {
    std::env::var_os("ALT_READ_PAST_DAMAGE").is_some()
}

/// What the opens of this process passed over as damaged so far, each
/// stretch once.
pub fn passed_damage() -> Vec<PassedDamage> {
    PASSED.lock().unwrap().clone()
}

pub(crate) fn note(file: &Path, from: u64, to: u64) {
    let passed = PassedDamage {
        file: file.to_owned(),
        from,
        to,
    };
    let mut all = PASSED.lock().unwrap();
    if !all.contains(&passed) {
        all.push(passed);
    }
}

/// Where the next record starts at or after `from` that reads: every byte
/// is tried as the start of a header, and where it could be one, its
/// payload is decoded and hashed. A record stored as a delta is not taken
/// for a start: it reads only against another.
fn next_whole(data: &[u8], from: usize) -> Option<usize> {
    (from..data.len().saturating_sub(REC_HEADER_LEN)).find(|&at| {
        let header: &[u8; REC_HEADER_LEN] = data[at..at + REC_HEADER_LEN].try_into().unwrap();
        let header = RecordHeader::parse(header);
        let payload = at + REC_HEADER_LEN;
        let payload = data.get(payload..payload + header.stored_len as usize);
        let decoded = payload.and_then(|payload| match header.encoding {
            ENC_RAW => Some(payload.to_vec()),
            ENC_ZSTD => alt_zstd::decode_all(payload).ok(),
            _ => None,
        });
        let sized = decoded.filter(|data| data.len() == header.orig_len as usize);
        sized.is_some_and(|data| ChunkStore::check_hash(header.id, &data).is_ok())
    })
}

/// Walks the pack at `path` on past the damage a walk stopped at, at
/// `stop`: from each next record that reads to where the walk stops
/// again. The records found are added to `recs`. Returns where the last
/// walk stopped: at the end of the pack, or where nothing reads after.
pub(crate) fn walk_on(
    path: &Path,
    read: &File,
    mut stop: u64,
    recs: &mut Vec<(RecordHeader, u64)>,
) -> Result<u64, StoreError> {
    // SAFETY: packs are append-only, so the mapped range never changes
    // under the map
    let map = unsafe { Mmap::map(read)? };
    let len = map.len() as u64;
    while stop < len {
        let Some(next) = next_whole(&map, stop as usize + 1) else {
            note(path, stop, len);
            break;
        };
        note(path, stop, next as u64);
        let (more, valid_len) = pack::scan_from(&map, next);
        recs.extend(more);
        stop = valid_len;
    }
    Ok(stop)
}
