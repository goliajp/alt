//! Advisory locks on the log file.

use std::fs::File;

/// Takes an exclusive advisory lock on the log file for the duration of one
/// append. `flock` is per-open-file-description and auto-releases when the fd
/// closes (incl. a crash), so there are no stale lock files. Non-unix has no
/// cross-process lock yet — single-writer there (documented limitation).
#[cfg(unix)]
pub(crate) fn lock_exclusive(file: &File) -> std::io::Result<()> {
    alt_os::sys::flock(file, alt_os::sys::LOCK_EX)
}

#[cfg(unix)]
pub(crate) fn lock_shared(file: &File) -> std::io::Result<()> {
    alt_os::sys::flock(file, alt_os::sys::LOCK_SH)
}

#[cfg(unix)]
pub(crate) fn unlock(file: &File) -> std::io::Result<()> {
    alt_os::sys::flock(file, alt_os::sys::LOCK_UN)
}

#[cfg(not(unix))]
pub(crate) fn lock_exclusive(_file: &File) -> std::io::Result<()> {
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn lock_shared(_file: &File) -> std::io::Result<()> {
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn unlock(_file: &File) -> std::io::Result<()> {
    Ok(())
}
