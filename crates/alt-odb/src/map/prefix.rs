//! Looking objects up by the first digits of their id.

use super::{MapEntry, ObjectMap, lift};
use crate::{NativeOdb, OdbError};

impl ObjectMap {
    /// Entries whose git oid starts with the given hex digits (lowercase),
    /// one for each oid, at most `cap` of them.
    pub fn by_prefix(&self, hex: &str, cap: usize) -> Result<Vec<MapEntry>, OdbError> {
        let (mut lo, mut hi) = ([0u8; 32], [0xffu8; 32]);
        for (i, digit) in hex.bytes().enumerate() {
            let nibble = (digit as char).to_digit(16).unwrap() as u8;
            let shift = if i % 2 == 0 { 4 } else { 0 };
            lo[i / 2] |= nibble << shift;
            hi[i / 2] &= !(0xf << shift) | nibble << shift;
        }
        let mut out: Vec<MapEntry> = Vec::new();
        // the padded key range also holds shorter oids ending in zeros
        for rec in self.log.range(&lo, &hi, usize::MAX).map_err(lift)? {
            let entry = MapEntry::parse(&rec)?;
            if entry.git.to_string().starts_with(hex) {
                out.push(entry);
            }
        }
        // the range comes run by run and then the tail, not in order of
        // id, and an object two writers stored at once has a record from
        // each: in order, those lie next to each other
        out.sort_by_key(|entry| entry.git);
        out.dedup_by_key(|entry| entry.git);
        out.truncate(cap);
        Ok(out)
    }
}

impl NativeOdb {
    /// Resolve a short hex prefix (≥ 4 hex chars) to the git oids it
    /// matches, at most `cap` of them so callers can detect ambiguity
    /// without paying for every match. Too-short or non-hex prefixes match
    /// nothing.
    pub fn lookup_by_prefix(&self, prefix: &str, cap: usize) -> Result<Vec<MapEntry>, OdbError> {
        self.odb()?.lookup_by_prefix(prefix, cap)
    }
}
