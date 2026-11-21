//! The boot appends to the log were left unsynced in.
//!
//! A writer that leaves its appends unsynced marks the synced marker so.
//! For as long as the machine stays up, what it appended is in the
//! kernel's cache as it was written: a record that does not read with
//! whole records after it is damage. After another boot the log may hold
//! anything past the boundary, a stretch that never reached the disk
//! with records after it that did: the first record that does not read
//! there ends the log, as past a boundary that holds.
//!
//! So the writer notes the running boot in `sync.lock` before it marks
//! the marker, past the byte of flags:
//!
//! ```text
//! [8..24) boot  [24..32) checksum = blake3(boot)[..8]
//! ```
//!
//! It is not fsynced: a note that was lost reads as another boot's. An
//! older alt only locks the file.

use std::fs::{File, OpenOptions};
use std::os::unix::fs::FileExt;
use std::path::Path;

const AT: u64 = 8;

/// `sync.lock`, created where it is not there, with what it holds left.
pub(crate) fn open_sync_lock(path: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
}

fn encode(boot: [u8; 16]) -> [u8; 24] {
    let mut note = [0u8; 24];
    note[..16].copy_from_slice(&boot);
    note[16..].copy_from_slice(&alt_hash::blake3::hash(&boot).as_bytes()[..8]);
    note
}

/// Notes, beside the marker at `durable`, that appends are left unsynced
/// in the running boot from now on.
pub(crate) fn note(durable: &Path) -> std::io::Result<()> {
    let note = alt_os::boot::id().map_or([0; 24], encode);
    open_sync_lock(&durable.with_file_name("sync.lock"))?.write_all_at(&note, AT)
}

/// Whether what was left unsynced in the log beside the marker at
/// `durable` was appended in the running boot, by what the note says.
pub(crate) fn cached(durable: &Path) -> bool {
    let mut note = [0u8; 24];
    let read = File::open(durable.with_file_name("sync.lock"))
        .and_then(|lock| lock.read_exact_at(&mut note, AT));
    read.is_ok() && alt_os::boot::id().is_some_and(|boot| encode(boot) == note)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_note_tells_of_the_running_boot_until_a_byte_of_it_is_another() {
        let dir = alt_os::temp::tempdir().unwrap();
        let durable = dir.path().join("durable");
        assert!(!cached(&durable));
        std::fs::write(dir.path().join("sync.lock"), [1]).unwrap();
        assert!(!cached(&durable));
        note(&durable).unwrap();
        assert!(cached(&durable));
        // the byte of flags before it is left as it was
        let lock = dir.path().join("sync.lock");
        let written = std::fs::read(&lock).unwrap();
        assert_eq!((written[0], written.len()), (1, 32));
        for at in 8..32 {
            let mut other = written.clone();
            other[at] ^= 1;
            std::fs::write(&lock, other).unwrap();
            assert!(!cached(&durable), "byte {at}");
        }
    }
}
