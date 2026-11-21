//! A note, beside the index, that the index file was whole when it was
//! last read or written, and which recovery snapshot holds its bytes.
//!
//! Reading the index checks the checksum it ends in, which is what tells
//! a file that a power cut tore from an index. A file that this boot of
//! the machine saw whole is whole for as long as it is the same file: the
//! same inode with the same size and times. For such a file the checksum
//! is not computed again, and the snapshot of it is known by name without
//! hashing it. Only a command that writes in the workspace notes it. A reboot, another file in its place, or a write into it
//! makes the note say nothing.
//!
//! The note is a cache: one that is missing or torn, or was written by
//! another boot, only costs the checksum. It trusts the times of the
//! file: a write into the index file in the same tick of the clock as the
//! last change of its inode is not seen. alt replaces the index and never
//! writes into it.

use std::fs::Metadata;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

const MAGIC: [u8; 8] = *b"ALTISEE\x01";
const SUM: usize = 8;
/// magic, the boot, seven numbers of the file, the snapshot, the sum
const LEN: usize = 8 + 16 + 7 * 8 + 32 + SUM;

/// The file an index was read from: its device and inode, what it is
/// told by from itself after a change, and the recovery snapshot noted
/// as holding its bytes.
pub(in crate::native) struct FromFile {
    pub(in crate::native) file: (u64, u64),
    state: [u8; 56],
    pub(in crate::native) snapshot: Option<String>,
}

impl FromFile {
    pub(super) fn of(meta: &Metadata, snapshot: Option<[u8; 32]>) -> Self {
        let hex = |id: [u8; 32]| id.iter().map(|byte| format!("{byte:02x}")).collect();
        Self {
            file: (meta.dev(), meta.ino()),
            state: file(meta),
            snapshot: snapshot.map(hex),
        }
    }

    /// Whether `meta` describes the file as it was when it was read:
    /// the same inode, with the size and the times it had.
    pub(in crate::native) fn is(&self, meta: &Metadata) -> bool {
        self.state == file(meta)
    }
}

/// The 32 bytes a snapshot id names.
pub(super) fn unhex(id: &str) -> Option<[u8; 32]> {
    let mut bytes = [0; 32];
    if id.len() != 64 {
        return None;
    }
    for (at, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(id.get(at * 2..at * 2 + 2)?, 16).ok()?;
    }
    Some(bytes)
}

pub(super) fn place(index_path: &Path) -> PathBuf {
    index_path.with_file_name("index.seen")
}

/// What the file is told from another, or from itself after a change.
fn file(meta: &Metadata) -> [u8; 56] {
    let numbers = [
        meta.dev(),
        meta.ino(),
        meta.size(),
        meta.mtime() as u64,
        meta.mtime_nsec() as u64,
        meta.ctime() as u64,
        meta.ctime_nsec() as u64,
    ];
    let mut bytes = [0; 56];
    for (at, number) in numbers.iter().enumerate() {
        bytes[at * 8..at * 8 + 8].copy_from_slice(&number.to_le_bytes());
    }
    bytes
}

fn encode(boot: [u8; 16], meta: &Metadata, snapshot: Option<[u8; 32]>) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(LEN);
    bytes.extend_from_slice(&MAGIC);
    bytes.extend_from_slice(&boot);
    bytes.extend_from_slice(&file(meta));
    bytes.extend_from_slice(&snapshot.unwrap_or_default());
    let sum = alt_hash::blake3::hash(&bytes);
    bytes.extend_from_slice(&sum.as_bytes()[..SUM]);
    bytes
}

/// Whether `note` says the file `meta` describes was seen whole in the
/// boot `boot`, and then the snapshot that holds its bytes where the
/// note names one.
fn decode(note: &[u8], boot: [u8; 16], meta: &Metadata) -> Option<Option<[u8; 32]>> {
    let (body, sum) = note.get(..LEN)?.split_at(LEN - SUM);
    let seen = sum == &alt_hash::blake3::hash(body).as_bytes()[..SUM]
        && body[..8] == MAGIC
        && body[8..24] == boot
        && body[24..80] == file(meta);
    let snapshot: [u8; 32] = body[80..112].try_into().unwrap();
    seen.then_some((snapshot != [0; 32]).then_some(snapshot))
}

/// Whether the index file `meta` describes, read from `index_path`, is
/// noted as seen whole in this boot; and the snapshot noted for it.
pub(super) fn seen(index_path: &Path, meta: &Metadata) -> Option<Option<[u8; 32]>> {
    let boot = alt_os::boot::id()?;
    decode(&std::fs::read(place(index_path)).ok()?, boot, meta)
}

/// The note that the index file `meta` describes is whole, and that
/// `snapshot` holds its bytes where one is given; `None` where the
/// kernel tells no boot. It is written whole and in place once the
/// command has succeeded: a reader that meets half of it finds a sum
/// that does not match.
pub(super) fn note(meta: &Metadata, snapshot: Option<[u8; 32]>) -> Option<Vec<u8>> {
    Some(encode(alt_os::boot::id()?, meta, snapshot))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_note_speaks_of_one_state_of_one_file_in_one_boot() {
        let dir = alt_os::temp::tempdir().unwrap();
        let index = dir.path().join("index");
        std::fs::write(&index, b"an index").unwrap();
        let meta = std::fs::metadata(&index).unwrap();
        let (boot, snapshot) = ([7; 16], [9; 32]);
        let note = encode(boot, &meta, Some(snapshot));
        assert_eq!(note.len(), LEN);
        assert_eq!(decode(&note, boot, &meta), Some(Some(snapshot)));
        assert_eq!(decode(&encode(boot, &meta, None), boot, &meta), Some(None));
        // another boot
        assert_eq!(decode(&note, [8; 16], &meta), None);
        // another file with the same bytes
        let other = dir.path().join("other");
        std::fs::write(&other, b"an index").unwrap();
        assert_eq!(
            decode(&note, boot, &std::fs::metadata(&other).unwrap()),
            None
        );
        // the same file, longer
        std::fs::write(&index, b"an index, changed").unwrap();
        assert_eq!(
            decode(&note, boot, &std::fs::metadata(&index).unwrap()),
            None
        );
        // every torn or altered form of the note says nothing
        for cut in 0..LEN {
            assert_eq!(decode(&note[..cut], boot, &meta), None, "cut at {cut}");
            let mut flipped = note.clone();
            flipped[cut] ^= 1;
            assert_eq!(decode(&flipped, boot, &meta), None, "bit at {cut}");
        }
    }

    #[test]
    fn what_was_noted_is_seen_until_the_file_is_another() {
        let dir = alt_os::temp::tempdir().unwrap();
        let index = dir.path().join("index");
        std::fs::write(&index, b"an index").unwrap();
        let meta = std::fs::metadata(&index).unwrap();
        assert_eq!(seen(&index, &meta), None);
        std::fs::write(place(&index), note(&meta, Some([3; 32])).unwrap()).unwrap();
        assert_eq!(seen(&index, &meta), Some(Some([3; 32])));
        let read = FromFile::of(&meta, None);
        assert!(read.is(&std::fs::metadata(&index).unwrap()));
        // replaced by a rename, as the index is
        let next = dir.path().join("next");
        std::fs::write(&next, b"an index").unwrap();
        std::fs::rename(&next, &index).unwrap();
        let replaced = std::fs::metadata(&index).unwrap();
        assert_eq!(seen(&index, &replaced), None);
        assert!(!read.is(&replaced));
    }
}
