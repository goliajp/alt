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
    pub flags: u32,
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
        flags: u32::from_le_bytes(body[16..20].try_into().unwrap()),
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

/// Sets the flag that says unsynced appends may follow the boundaries, as
/// a writer that skips its fsyncs does.
pub fn distrust(alt_dir: &Path) {
    let path = alt_dir.join("odb.durable");
    let mut data = fs::read(&path).unwrap();
    let serial = recorded(alt_dir).serial;
    let at = [32, 128]
        .into_iter()
        .find(|at| slot(&data[*at..*at + 96]).is_some_and(|slot| slot.serial == serial))
        .unwrap();
    data[at + 16] |= 1;
    let sum = alt_hash::blake3::hash(&data[at..at + 88]);
    data[at + 88..at + 96].copy_from_slice(&sum.as_bytes()[..8]);
    fs::write(&path, data).unwrap();
}

/// Replaces what the marker says with `change` applied to it, as the next
/// write of the store would: with the next serial.
pub fn rewrite(alt_dir: &Path, change: impl FnOnce(&mut Recorded)) {
    let path = alt_dir.join("odb.durable");
    let mut data = fs::read(&path).unwrap();
    let mut state = recorded(alt_dir);
    let at = [32, 128]
        .into_iter()
        .find(|at| slot(&data[*at..*at + 96]).is_some_and(|slot| slot.serial == state.serial))
        .unwrap();
    change(&mut state);
    let body = &mut data[at..at + 96];
    body[..8].copy_from_slice(&(state.serial + 1).to_le_bytes());
    body[8..16].copy_from_slice(&state.generation.to_le_bytes());
    body[16..20].copy_from_slice(&state.flags.to_le_bytes());
    for (n, len) in state.eof.iter().chain(&state.whole).enumerate() {
        body[24 + 8 * n..32 + 8 * n].copy_from_slice(&len.to_le_bytes());
    }
    let sum = alt_hash::blake3::hash(&body[..88]);
    body[88..].copy_from_slice(&sum.as_bytes()[..8]);
    fs::write(&path, data).unwrap();
}
