//! Advisory locks on the odb's lock files.

use std::fs::File;

/// Takes an exclusive advisory lock on the odb write-lock file for the duration
/// of one write batch. `flock` is per-open-file-description and auto-releases on
/// close/crash, so there are no stale locks. Non-unix has no cross-process lock
/// yet — single-writer there (documented limitation, like the op log).
#[cfg(unix)]
pub(crate) fn lock_exclusive(file: &File) -> std::io::Result<()> {
    alt_os::sys::flock(file, alt_os::sys::LOCK_EX)
}

/// Shared (read) lock — taken while opening, so the open's reads + crash
/// recovery of the append files can't overlap an exclusive writer's append.
/// Multiple opens share it; an exclusive writer waits for them and vice versa.
#[cfg(unix)]
pub(crate) fn lock_shared(file: &File) -> std::io::Result<()> {
    alt_os::sys::flock(file, alt_os::sys::LOCK_SH)
}

#[cfg(unix)]
pub(crate) fn unlock(file: &File) -> std::io::Result<()> {
    alt_os::sys::flock(file, alt_os::sys::LOCK_UN)
}

#[cfg(not(unix))]
pub(crate) fn lock_shared(_file: &File) -> std::io::Result<()> {
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn lock_exclusive(_file: &File) -> std::io::Result<()> {
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn unlock(_file: &File) -> std::io::Result<()> {
    Ok(())
}

/// Takes the exclusive lock only if no one holds any lock on the file;
/// reports whether it was taken.
#[cfg(unix)]
pub(crate) fn try_lock_exclusive(file: &File) -> std::io::Result<bool> {
    match alt_os::sys::flock(file, alt_os::sys::LOCK_EX | alt_os::sys::LOCK_NB) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(false),
        Err(e) => Err(e),
    }
}

#[cfg(not(unix))]
pub(crate) fn try_lock_exclusive(_file: &File) -> std::io::Result<bool> {
    Ok(true)
}
