//! Reads the log's `durable` marker byte by byte, for tests that look at
//! what the log recorded as synced.

use std::fs;
use std::path::Path;

/// One slot of the marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Recorded {
    pub serial: u64,
    pub flags: u32,
    pub eof: u64,
    pub whole: u64,
}

fn slot(slot: &[u8]) -> Option<Recorded> {
    let (body, sum) = slot.split_at(28);
    if alt_hash::blake3::hash(body).as_bytes()[..8] != *sum {
        return None;
    }
    let word = |at: usize| u64::from_le_bytes(body[at..at + 8].try_into().unwrap());
    Some(Recorded {
        serial: word(0),
        flags: u32::from_le_bytes(body[8..12].try_into().unwrap()),
        eof: word(12),
        whole: word(20),
    })
}

/// What the marker in `dir` says: its valid slot with the larger serial.
pub fn recorded(dir: &Path) -> Recorded {
    let data = fs::read(dir.join("durable")).unwrap();
    assert_eq!(&data[8..12], b"ALT2");
    [16, 52]
        .into_iter()
        .filter_map(|at| slot(data.get(at..at + 36)?))
        .max_by_key(|slot| slot.serial)
        .expect("no slot of the marker holds")
}

/// Sets the flag that says unsynced appends may follow the boundary, as a
/// writer that skips its fsyncs does.
pub fn distrust(dir: &Path) {
    let path = dir.join("durable");
    let mut data = fs::read(&path).unwrap();
    let serial = recorded(dir).serial;
    let at = [16, 52]
        .into_iter()
        .find(|at| slot(&data[*at..*at + 36]).is_some_and(|slot| slot.serial == serial))
        .unwrap();
    data[at + 8] |= 1;
    let sum = alt_hash::blake3::hash(&data[at..at + 28]);
    data[at + 28..at + 36].copy_from_slice(&sum.as_bytes()[..8]);
    fs::write(&path, data).unwrap();
}
