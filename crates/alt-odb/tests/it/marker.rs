//! Reads `odb.durable` byte by byte, for tests that look at what the store
//! recorded as synced.

use std::fs;
use std::path::Path;

/// One slot of the marker; the lengths are of the active pack, the blob
/// map, the Tier 1 map and `map.alt`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Recorded {
    pub serial: u64,
    pub generation: u64,
    pub eof: [u64; 4],
    pub whole: [u64; 4],
}

fn slot(slot: &[u8]) -> Option<Recorded> {
    let (body, sum) = slot.split_at(88);
    if alt_hash::blake3::hash(body).as_bytes()[..8] != *sum {
        return None;
    }
    let word = |at: usize| u64::from_le_bytes(body[at..at + 8].try_into().unwrap());
    Some(Recorded {
        serial: word(0),
        generation: word(8),
        eof: [24, 32, 40, 48].map(word),
        whole: [56, 64, 72, 80].map(word),
    })
}

/// What the marker under `alt_dir` says: its valid slot with the larger
/// serial.
pub fn recorded(alt_dir: &Path) -> Recorded {
    let data = fs::read(alt_dir.join("odb.durable")).unwrap();
    assert_eq!(&data[24..28], b"ALT2");
    [32, 128]
        .into_iter()
        .filter_map(|at| slot(data.get(at..at + 96)?))
        .max_by_key(|slot| slot.serial)
        .expect("no slot of the marker holds")
}
