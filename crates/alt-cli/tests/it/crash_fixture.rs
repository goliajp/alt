//! What a crash leaves, for the tests of what comes after one.

use std::path::Path;

/// Takes the last record off the operation log as a crash before its sync
/// leaves it: the marker says the log was synced up to the record before.
pub(super) fn remove_final_log_frame(root: &Path) {
    let path = root.join(".alt/oplog/log");
    let bytes = std::fs::read(&path).unwrap();
    let mut cursor = 5;
    let mut last = 5;
    while cursor < bytes.len() {
        last = cursor;
        let len = u32::from_le_bytes(bytes[cursor..cursor + 4].try_into().unwrap()) as usize;
        cursor += 4 + len + 8;
    }
    std::fs::write(path, &bytes[..last]).unwrap();
    // serial, flags, length at the last sync, synced boundary, checksum
    let mut slot = [
        &1u64.to_le_bytes()[..],
        &[0; 4],
        &(last as u64).to_le_bytes(),
    ]
    .concat();
    slot.extend_from_slice(&(last as u64).to_le_bytes());
    let sum = alt_hash::blake3::hash(&slot);
    let marker = [
        &[0; 8][..],
        b"ALT2",
        &[0; 4],
        &slot,
        &sum.as_bytes()[..8],
        &[0; 36],
    ]
    .concat();
    std::fs::write(root.join(".alt/oplog/durable"), marker).unwrap();
}
