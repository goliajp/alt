//! Records already read from behind the index runs, kept by key.
//!
//! A record behind the runs is read from the file, and its checksum
//! verified, each time it is looked up. A process that looks the same
//! records up over and over (a daemon serving one status after another
//! reads the same trees each time) would pay that on every lookup where a
//! log replayed into memory paid nothing; such a process turns this on and
//! the records it has read stay here. A command that runs once and exits
//! leaves it off and allocates nothing.

use std::sync::Mutex;

use alt_hash::IdMap;

use super::{MAX_REC, Record};
use crate::runs::Key;

/// Records kept before the table is emptied and filled again: about 30
/// megabytes, several times the trees of a very large working tree.
const CAPACITY: usize = 1 << 18;

#[derive(Default)]
pub(super) struct ReadCache {
    on: bool,
    kept: Mutex<Kept>,
}

/// The records in the order they were first read, and where each key's
/// record is among them. A process that repeats a walk reads them back in
/// the order it kept them, one after the other in memory.
#[derive(Default)]
struct Kept {
    at: IdMap<Key, u32>,
    records: Vec<u8>,
}

impl ReadCache {
    pub fn enable(&mut self) {
        self.on = true;
    }

    pub fn get(&self, key: &Key, rec_len: usize) -> Option<Record> {
        if !self.on {
            return None;
        }
        let kept = self.kept.lock().unwrap();
        let at = *kept.at.get(key)? as usize * rec_len;
        let mut buf = [0u8; MAX_REC];
        buf[..rec_len].copy_from_slice(&kept.records[at..at + rec_len]);
        Some(Record { buf, len: rec_len })
    }

    /// Keeps a record just read from the file.
    pub fn put(&self, key: Key, record: &Record) {
        if !self.on {
            return;
        }
        let mut kept = self.kept.lock().unwrap();
        if kept.at.len() >= CAPACITY {
            kept.at.clear();
            kept.records.clear();
        }
        let at = (kept.records.len() / record.len) as u32;
        kept.records.extend_from_slice(record);
        kept.at.insert(key, at);
    }

    /// Forgets everything: the log was cut back, or records moved from the
    /// tail into the runs (a key written again has a newer record there
    /// than the one kept here).
    pub fn clear(&mut self) {
        *self.kept.get_mut().unwrap() = Kept::default();
    }
}
