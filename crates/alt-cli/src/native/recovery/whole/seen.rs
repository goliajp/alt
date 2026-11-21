//! A note, beside the index, that the index file was whole when it was
//! last read or written, and which recovery snapshot holds its bytes.
//!
//! Reading the index checks the checksum it ends in, which is what tells
//! a file that a power cut tore from an index. A file that this boot of
//! the machine saw whole is whole for as long as it is the same file: the
//! same inode with the same size and times. For such a file the checksum
//! is not computed again, and the snapshot of it is known by name without
//! hashing it. Only a command that writes in the workspace notes it. A
//! reboot, another file in its place, or a write into it makes the note
//! say nothing.
//!
//! The note also says whether every blob the index stages was found in
//! the store in this boot. What a boot wrote into the store stays there
//! until the machine goes down, so an add that starts from such an index
//! does not look for staged blobs the store lost, and the index it leaves
//! is such an index again: it stages what the one before staged and what
//! the add has just stored.
//!
//! The note is a cache: one that is missing or torn, or was written by
//! another boot, only costs the checksum. It trusts the times of the
//! file: a write into the index file in the same tick of the clock as the
//! last change of its inode is not seen. alt replaces the index and never
//! writes into it.

use std::fs::Metadata;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

const MAGIC: [u8; 8] = *b"ALTISEE\x02";
const SUM: usize = 8;
/// magic, the boot, seven numbers of the file, the snapshot, whether
/// what it stages is stored, the sum
const LEN: usize = 8 + 16 + 7 * 8 + 32 + 8 + SUM;
/// where the note says whether what the index stages is stored
const STORED: usize = 112;

/// What a note says of the index file it is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Seen {
    /// the recovery snapshot that holds its bytes
    pub(super) snapshot: Option<[u8; 32]>,
    /// every blob it stages was in the store in this boot
    pub(super) stored: bool,
}

/// The file an index was read from: its device and inode, what it is
/// told by from itself after a change, and the recovery snapshot noted
/// as holding its bytes.
#[derive(Clone)]
pub(in crate::native) struct FromFile {
    pub(in crate::native) file: (u64, u64),
    state: [u8; 56],
    pub(in crate::native) snapshot: Option<String>,
    /// every blob the index stages was in the store in this boot
    pub(in crate::native) stored: bool,
}

impl FromFile {
    pub(super) fn of(meta: &Metadata, seen: Option<Seen>) -> Self {
        let hex = |id: [u8; 32]| id.iter().map(|byte| format!("{byte:02x}")).collect();
        Self {
            file: (meta.dev(), meta.ino()),
            state: file(meta),
            snapshot: seen.and_then(|seen| seen.snapshot).map(hex),
            stored: seen.is_some_and(|seen| seen.stored),
        }
    }

    /// The note that the file, as it was read, is whole and stages only
    /// what is stored; `None` where the kernel tells no boot.
    fn note_stored(&self) -> Option<Vec<u8>> {
        let seen = Seen {
            snapshot: self.snapshot.as_deref().and_then(unhex),
            stored: true,
        };
        Some(encode(alt_os::boot::id()?, self.state, seen))
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

fn encode(boot: [u8; 16], file: [u8; 56], seen: Seen) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(LEN);
    bytes.extend_from_slice(&MAGIC);
    bytes.extend_from_slice(&boot);
    bytes.extend_from_slice(&file);
    bytes.extend_from_slice(&seen.snapshot.unwrap_or_default());
    bytes.extend_from_slice(&u64::from(seen.stored).to_le_bytes());
    seal(&mut bytes);
    bytes
}

fn seal(body: &mut Vec<u8>) {
    let sum = alt_hash::blake3::hash(body);
    body.extend_from_slice(&sum.as_bytes()[..SUM]);
}

/// Has `note`, which this process made, say as well that what its index
/// stages is stored.
fn say_stored(note: &mut Vec<u8>) {
    note.truncate(LEN - SUM);
    note[STORED..].copy_from_slice(&1u64.to_le_bytes());
    seal(note);
}

/// Has `noted`, the note a command leaves once it has succeeded, say
/// that the index at `index_path` stages only what is stored: the index
/// the command put there and noted, or with `read` the file it read and
/// left.
pub(in crate::native) fn note_stored(
    noted: &mut Option<(PathBuf, Vec<u8>)>,
    index_path: &Path,
    read: Option<&FromFile>,
) {
    match read {
        Some(read) => *noted = read.note_stored().map(|note| (place(index_path), note)),
        None => {
            if let Some((_, note)) = noted {
                say_stored(note);
            }
        }
    }
}

/// What `note` says of the file `meta` describes, where it says that
/// the file was seen whole in the boot `boot`.
fn decode(note: &[u8], boot: [u8; 16], meta: &Metadata) -> Option<Seen> {
    let (body, sum) = note.get(..LEN)?.split_at(LEN - SUM);
    let seen = sum == &alt_hash::blake3::hash(body).as_bytes()[..SUM]
        && body[..8] == MAGIC
        && body[8..24] == boot
        && body[24..80] == file(meta);
    let snapshot: [u8; 32] = body[80..STORED].try_into().unwrap();
    seen.then_some(Seen {
        snapshot: (snapshot != [0; 32]).then_some(snapshot),
        stored: body[STORED..] == 1u64.to_le_bytes(),
    })
}

/// What is noted of the index file `meta` describes, read from
/// `index_path`, where it is noted as seen whole in this boot.
pub(super) fn seen(index_path: &Path, meta: &Metadata) -> Option<Seen> {
    let boot = alt_os::boot::id()?;
    decode(&std::fs::read(place(index_path)).ok()?, boot, meta)
}

/// The note that the index file `meta` describes is whole, and that
/// `snapshot` holds its bytes where one is given; `None` where the
/// kernel tells no boot. It is written whole and in place once the
/// command has succeeded: a reader that meets half of it finds a sum
/// that does not match.
pub(super) fn note(meta: &Metadata, snapshot: Option<[u8; 32]>) -> Option<Vec<u8>> {
    let stored = false;
    Some(encode(
        alt_os::boot::id()?,
        file(meta),
        Seen { snapshot, stored },
    ))
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
        let seen = |snapshot, stored| Seen { snapshot, stored };
        let mut note = encode(boot, file(&meta), seen(Some(snapshot), false));
        assert_eq!(note.len(), LEN);
        assert_eq!(
            decode(&note, boot, &meta),
            Some(seen(Some(snapshot), false))
        );
        let bare = encode(boot, file(&meta), seen(None, false));
        assert_eq!(decode(&bare, boot, &meta), Some(seen(None, false)));
        // what it stages is stored only where the note was made to say so
        say_stored(&mut note);
        assert_eq!(note.len(), LEN);
        assert_eq!(decode(&note, boot, &meta), Some(seen(Some(snapshot), true)));
        let read = FromFile::of(&meta, decode(&note, boot, &meta));
        assert!(read.stored);
        assert!(!FromFile::of(&meta, None).stored);
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
        let noted = seen(&index, &meta).unwrap();
        assert_eq!((noted.snapshot, noted.stored), (Some([3; 32]), false));
        let read = FromFile::of(&meta, Some(noted));
        // the note the file as read makes says the same of it, and that
        // what it stages is stored
        std::fs::write(place(&index), read.note_stored().unwrap()).unwrap();
        let noted = seen(&index, &meta).unwrap();
        assert_eq!((noted.snapshot, noted.stored), (Some([3; 32]), true));
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
