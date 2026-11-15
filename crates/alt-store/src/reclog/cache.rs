//! Records already read from behind the index runs, kept by key.
//!
//! A record behind the runs is read from the file each time it is looked
//! up. A process that looks the same records up over and over (a daemon
//! serving one status after another reads the same trees each time) would
//! pay a system call per lookup where a log replayed into memory paid
//! none; the ones it has read stay here instead.
//!
//! One slot per key prefix, the newer record replacing whatever was there,
//! so keeping a record costs a copy. A process that reads only a few
//! records keeps none and allocates nothing.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use super::{MAX_REC, Record};
use crate::runs::{KEY_LEN, Key};

/// Slots; a full table is about four megabytes of zeroed memory that is
/// touched only where a record lands.
const SLOTS: usize = 1 << 15;
/// One slot: the key, the record's length (zero while empty), the record.
const SLOT_LEN: usize = KEY_LEN + 8 + MAX_REC;
/// Reads from the file before the table is worth having.
const WARM_UP: u32 = 16;

#[derive(Default)]
pub(super) struct ReadCache {
    reads: AtomicU32,
    slots: Mutex<Vec<u8>>,
}

/// Keys are digests: any eight bytes of one spread evenly.
fn slot(key: &Key) -> usize {
    u64::from_le_bytes(key[..8].try_into().unwrap()) as usize % SLOTS * SLOT_LEN
}

impl ReadCache {
    pub fn get(&self, key: &Key) -> Option<Record> {
        let slots = self.slots.lock().unwrap();
        let held = slots.get(slot(key)..)?;
        let len = u64::from_le_bytes(held[KEY_LEN..KEY_LEN + 8].try_into().unwrap()) as usize;
        if len == 0 || held[..KEY_LEN] != key[..] {
            return None;
        }
        let mut buf = [0u8; MAX_REC];
        buf.copy_from_slice(&held[KEY_LEN + 8..SLOT_LEN]);
        Some(Record { buf, len })
    }

    /// Keeps a record just read from the file, once the process has read
    /// enough of them to look like one that will ask again.
    pub fn put(&self, key: Key, record: &Record) {
        if self.reads.fetch_add(1, Ordering::Relaxed) < WARM_UP {
            return;
        }
        let mut slots = self.slots.lock().unwrap();
        if slots.is_empty() {
            *slots = vec![0u8; SLOTS * SLOT_LEN];
        }
        let held = &mut slots[slot(&key)..][..SLOT_LEN];
        held[..KEY_LEN].copy_from_slice(&key);
        held[KEY_LEN..KEY_LEN + 8].copy_from_slice(&(record.len as u64).to_le_bytes());
        held[KEY_LEN + 8..].copy_from_slice(&record.buf);
    }

    /// Forgets everything: the log was cut back, or records moved from the
    /// tail into the runs (a key written again has a newer record there
    /// than the one kept here).
    pub fn clear(&mut self) {
        *self.slots.get_mut().unwrap() = Vec::new();
    }
}
