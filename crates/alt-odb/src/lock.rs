//! Advisory locks on the odb's lock files.

use std::fs::File;

use alt_store::held;

/// Takes an exclusive advisory lock on a lock file. `flock` is per
/// open-file-description and auto-releases on close/crash, so there are no
/// stale locks.
pub(crate) fn lock_exclusive(file: &File) -> std::io::Result<()> {
    alt_os::sys::flock(file, alt_os::sys::LOCK_EX)
}

/// Takes the odb write lock for one write batch and records this thread as
/// its holder, so a second handle on the same store that this thread opens
/// or reads through mid-batch does not wait for the batch to end.
pub(crate) fn hold_exclusive(file: &File) -> std::io::Result<()> {
    lock_exclusive(file)?;
    held::note_exclusive(file)
}

/// Shared (read) lock — taken while opening, so the open's reads + crash
/// recovery of the append files can't overlap an exclusive writer's append.
/// Multiple opens share it; an exclusive writer waits for them and vice versa.
pub(crate) fn lock_shared(file: &File) -> std::io::Result<()> {
    held::lock_shared(file)
}

pub(crate) fn unlock(file: &File) -> std::io::Result<()> {
    alt_os::sys::flock(file, alt_os::sys::LOCK_UN)
}

/// Gives up the lock [`hold_exclusive`] or [`try_hold_exclusive`] took.
pub(crate) fn release_exclusive(file: &File) {
    held::note_released(file);
    let _ = unlock(file);
}

/// Takes the odb write lock only if no one holds any lock on the file;
/// reports whether it was taken.
pub(crate) fn try_hold_exclusive(file: &File) -> std::io::Result<bool> {
    match alt_os::sys::flock(file, alt_os::sys::LOCK_EX | alt_os::sys::LOCK_NB) {
        Ok(()) => held::note_exclusive(file).map(|()| true),
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(false),
        Err(e) => Err(e),
    }
}
