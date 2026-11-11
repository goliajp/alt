//! SHA-256 of what the encoder writes for each test input, taken in a run
//! where the reference implementation decoded those very bytes back to the
//! input. If a change to the encoder alters its output, add the reference
//! implementation back as a dev-dependency, rerun `regenerate.rs` as its
//! header says, and store the new digests and frames.

use std::collections::HashMap;
use std::sync::OnceLock;

static LINES: &str = include_str!("frames/own.sha256");

fn table() -> &'static HashMap<&'static str, &'static str> {
    static TABLE: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    TABLE.get_or_init(|| {
        LINES
            .lines()
            .map(|line| {
                let (digest, key) = line.split_once("  ").unwrap();
                (key, digest)
            })
            .collect()
    })
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Number of digests on record.
pub fn count() -> usize {
    table().len()
}

/// Fails unless `frame` is byte for byte what was verified under `key`.
pub fn expect(key: &str, frame: &[u8]) {
    let recorded = table()
        .get(key)
        .unwrap_or_else(|| panic!("no digest recorded for {key}"));
    assert!(
        hex(&alt_hash::sha256(frame)) == *recorded,
        "{key}: encoder output differs from the verified bytes"
    );
}
