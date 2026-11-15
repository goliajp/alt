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

use super::Record;
use crate::runs::Key;

/// Records kept before the table is emptied and filled again: about 36
/// megabytes, several times the trees of a very large working tree.
const CAPACITY: usize = 1 << 18;

#[derive(Default)]
pub(super) struct ReadCache {
    on: bool,
    records: Mutex<IdMap<Key, Record>>,
}

impl ReadCache {
    pub fn enable(&mut self) {
        self.on = true;
    }

    pub fn get(&self, key: &Key) -> Option<Record> {
        if !self.on {
            return None;
        }
        self.records.lock().unwrap().get(key).copied()
    }

    /// Keeps a record just read from the file.
    pub fn put(&self, key: Key, record: &Record) {
        if !self.on {
            return;
        }
        let mut records = self.records.lock().unwrap();
        if records.len() >= CAPACITY {
            records.clear();
        }
        records.insert(key, *record);
    }

    /// Forgets everything: the log was cut back, or records moved from the
    /// tail into the runs (a key written again has a newer record there
    /// than the one kept here).
    pub fn clear(&mut self) {
        self.records.get_mut().unwrap().clear();
    }
}
