//! Advisory locks on the log file.

use std::fs::File;

use crate::OpLog;

impl OpLog {
    /// Runs `run` with the exclusive lock on the log taken. The lock is
    /// given up when `run` is over, also by a panic: what `run` calls may
    /// be the caller's, and the handle may live on in a process that goes
    /// on after it.
    pub(crate) fn locked<T>(&mut self, run: impl FnOnce(&mut Self) -> T) -> std::io::Result<T> {
        struct Locked<'a>(&'a mut OpLog);
        impl Drop for Locked<'_> {
            fn drop(&mut self) {
                // the lock is also dropped if the fd closes
                let _ = unlock(&self.0.file);
            }
        }
        lock_exclusive(&self.file)?;
        let locked = Locked(self);
        Ok(run(&mut *locked.0))
    }
}

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
