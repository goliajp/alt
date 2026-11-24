//! `tail`, the note beside a version 2 log, saying any number: the log
//! is read with it or without it, and never past what the file holds.

use std::fs;

use super::v2_fields::{values, written};
use super::v2_untrusted::{anchors, read_through, varied};

/// `tail` as it says `fields` of the log of `generation` in boot
/// `in_boot` (up to where it is synced, from where ops need vouching for,
/// how long the file is whose end is hidden, and where the log ends in
/// it), and whether ops are left to be looked at.
pub fn tail(in_boot: u8, generation: u32, pending: bool, fields: [u64; 4]) -> [u8; 72] {
    let mut bytes = [0u8; 72];
    bytes[..4].copy_from_slice(b"ALTT");
    bytes[4] = pending as u8;
    bytes[8..24].copy_from_slice(&[in_boot; 16]);
    bytes[24..28].copy_from_slice(&generation.to_le_bytes());
    for (n, field) in fields.iter().enumerate() {
        bytes[32 + n * 8..40 + n * 8].copy_from_slice(&field.to_le_bytes());
    }
    let sum = alt_hash::blake3::hash(&bytes[..64]);
    bytes[64..].copy_from_slice(&sum.as_bytes()[..8]);
    bytes
}

#[test]
fn a_note_beside_the_log_that_says_any_number_is_read_with_or_refused() {
    let dir = alt_os::temp::tempdir().unwrap();
    varied(dir.path());
    let (whole, noted) = written(dir.path());
    let anchors = anchors(dir.path());
    let word = |at: usize| u64::from_le_bytes(noted[at..at + 8].try_into().unwrap());
    let said = [word(32), word(40), word(48), word(56)];
    let len = whole.len() as u64;
    // with the header of the log telling of no cut, and of one that the
    // log does not hold
    let flags = [(false, false), (true, false), (false, true), (true, true)];
    let mut cases = 0;
    for (pending, cutting) in flags {
        let told = if cutting { (2, said[1]) } else { (0, 0) };
        super::v2_cut_told::tell_cut(dir.path(), told);
        for field in 0..5 {
            for to in values(len, said[1].min(len)) {
                // as a reader's note leaves it: of a file as long as this one
                let (mut generation, mut fields) = (1, [said[0], said[1], len, said[1]]);
                match field {
                    0 => generation = to as u32,
                    n => fields[n - 1] = to,
                }
                let note = tail(2, generation, pending, fields);
                fs::write(dir.path().join("tail"), note).unwrap();
                let case = format!("note field {field} = {to}, {pending} {cutting}");
                read_through(dir.path(), &anchors, (len, 0), &case);
                cases += 1;
            }
        }
    }
    assert_eq!(cases, 200);
}
