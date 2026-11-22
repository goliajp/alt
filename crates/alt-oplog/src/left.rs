//! Appends to the log that were left unsynced, and the boot they were
//! left unsynced in.
//!
//! A writer that leaves its appends unsynced marks the synced marker so.
//! Nothing past the boundary the marker then records was ever synced: a
//! record there that does not read ends the log, as past a boundary that
//! holds, and an op there that makes a ref point at an object which is
//! not stored was never confirmed ([`Marker::judged_from`]).
//!
//! The writer also notes the running boot in `sync.lock` before it marks
//! the marker, past the byte of flags. The full check goes by it: in the
//! running boot it tells a record that does not read there, with whole
//! ones after it, as damage that the next writer cuts off, and an older
//! alt takes what was appended in the running boot for whole.
//!
//! ```text
//! [8..24) boot  [24..32) checksum = blake3(boot)[..8]
//! ```
//!
//! It is not fsynced: a note that was lost reads as another boot's. An
//! alt older than that only locks the file.

use std::fs::{File, OpenOptions};
use std::os::unix::fs::FileExt;
use std::path::Path;

use crate::lock::{lock_exclusive, unlock};
use crate::synced::Marker;
use crate::{Op, OpLog, OpLogError, Unconfirmed};

const AT: u64 = 8;

impl Marker {
    /// From which byte on the ops of the log were never synced and are
    /// judged by what they name ([`crate::OpLog::settle`]), and whether
    /// appends were left unsynced past it; `None` where the marker has
    /// no boundary.
    pub(crate) fn judged_from(&self) -> Option<(u64, bool)> {
        match self.recorded() {
            Some(state) => Some((state.whole, !self.trusted())),
            None => self.unsynced_from().map(|from| (from, false)),
        }
    }
}

impl OpLog {
    /// [`Self::unconfirmed`] with `held` told whether the op lies past a
    /// boundary that appends were left unsynced past.
    pub fn unconfirmed_past<E: From<OpLogError>>(
        &self,
        mut held: impl FnMut(&Op, bool) -> Result<bool, E>,
    ) -> Result<Option<Unconfirmed>, E> {
        match Marker::read(&self.durable_path).judged_from() {
            Some((from, left)) if from < self.len_bytes => {
                self.first_unheld(from, |op| held(op, left))
            }
            _ => Ok(None),
        }
    }

    /// Whether a writer has ops to look at under the lock
    /// ([`Self::settle`]): ops past a boundary that holds, or, past one
    /// that appends were left unsynced past, an op `held` does not vouch
    /// for. Reads the marker once and takes no lock.
    pub fn unsettled_past<E: From<OpLogError>>(
        &self,
        held: impl FnMut(&Op) -> Result<bool, E>,
    ) -> Result<bool, E> {
        match Marker::read(&self.durable_path).judged_from() {
            Some((from, left)) if from < self.len_bytes => {
                Ok(!left || self.first_unheld(from, held)?.is_some())
            }
            _ => Ok(false),
        }
    }

    /// [`Self::settle`] with `held` told whether the op lies past a
    /// boundary that appends were left unsynced past.
    pub fn settle_past<E: From<OpLogError>>(
        &mut self,
        held: impl FnMut(&Op, bool) -> Result<bool, E>,
    ) -> Result<Option<Unconfirmed>, E> {
        lock_exclusive(&self.file).map_err(OpLogError::from)?;
        let settled = self.settle_locked(held);
        let _ = unlock(&self.file);
        settled
    }
}

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
