//! A note of how far the log is known synced, kept in a file readers of
//! the log open anyway, so that they need not read the marker.
//!
//! ```text
//! [length u64][checksum 8 = blake3(the 8 bytes before it)[..8]]
//! ```
//!
//! Whoever owns the file says where in it the note lies and how the file
//! begins when it has room for one ([`NotePlace`]). A writer raises the
//! note after the marker, without an fsync, so it can fall behind the
//! marker. It is never ahead of it, and nothing takes it back: no cut of
//! the log takes records the marker says were synced.

use std::fs::{File, OpenOptions};
use std::path::PathBuf;

pub const NOTE_LEN: usize = 16;

fn check(len: &[u8]) -> [u8; 8] {
    alt_hash::blake3::hash(len).as_bytes()[..8]
        .try_into()
        .unwrap()
}

/// The bytes of a note that the log is synced up to `len`.
pub fn encode_note(len: u64) -> [u8; NOTE_LEN] {
    let mut note = [0u8; NOTE_LEN];
    note[..8].copy_from_slice(&len.to_le_bytes());
    let sum = check(&note[..8]);
    note[8..].copy_from_slice(&sum);
    note
}

/// How far `note` says the log is synced; `None` for bytes that are no
/// note.
pub fn parse_note(note: &[u8]) -> Option<u64> {
    let (len, sum) = note.get(..NOTE_LEN)?.split_at(8);
    (check(len) == *sum).then(|| u64::from_le_bytes(len.try_into().unwrap()))
}

/// Where the note is kept: at byte `at` of the file at `path`, when that
/// file begins with `tag`.
#[derive(Debug, Clone)]
pub struct NotePlace {
    pub path: PathBuf,
    pub tag: Vec<u8>,
    pub at: u64,
}

impl NotePlace {
    /// Notes that the marker records the log as synced up to `len`.
    /// Failures are ignored: without the note a reader reads the marker.
    pub(crate) fn raise(&self, len: u64) {
        let Ok(file) = OpenOptions::new().read(true).write(true).open(&self.path) else {
            return;
        };
        if crate::read::read_at(&file, 0, self.tag.len()).is_ok_and(|tag| tag == self.tag) {
            let _ = write_at(&file, &encode_note(len), self.at);
        }
    }
}

#[cfg(unix)]
fn write_at(file: &File, bytes: &[u8], at: u64) -> std::io::Result<()> {
    std::os::unix::fs::FileExt::write_all_at(file, bytes, at)
}

#[cfg(not(unix))]
fn write_at(mut file: &File, bytes: &[u8], at: u64) -> std::io::Result<()> {
    use std::io::{Seek, SeekFrom, Write};
    file.seek(SeekFrom::Start(at))?;
    file.write_all(bytes)
}
