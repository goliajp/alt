//! `lock` beside the log.

use std::fs::{File, OpenOptions};
use std::io::ErrorKind;
use std::path::Path;

use crate::lock::{lock_exclusive, unlock};

/// `lock` beside the log, held by one at a time of those that fsync the
/// log to move the boundary, cut the log or write `tail`.
pub(crate) struct Held(File);

impl Held {
    pub fn take(dir: &Path) -> std::io::Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join("lock"))?;
        lock_exclusive(&file)?;
        Ok(Held(file))
    }

    /// Waits for whoever holds `lock` in `dir` and takes it, where the
    /// file is there: `None` where it is not, and nobody holds it then.
    /// For a reader, which makes nothing beside a log.
    pub fn after_its_holder(dir: &Path) -> std::io::Result<Option<Self>> {
        match OpenOptions::new().write(true).open(dir.join("lock")) {
            Ok(file) => {
                lock_exclusive(&file)?;
                Ok(Some(Held(file)))
            }
            Err(none) if none.kind() == ErrorKind::NotFound => Ok(None),
            Err(other) => Err(other),
        }
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        let _ = unlock(&self.0);
    }
}
