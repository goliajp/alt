//! Writing the store's own configuration file. As git writes a
//! configuration file: the new text goes into `config.lock`, made for
//! this writer alone, and takes the place of the file by a rename. A
//! second writer finds the lock and gives up; a reader sees the file
//! whole, before or after.

use std::fs;
use std::io::Write;
use std::path::Path;

use super::OWN;

/// Rewrites the store's own file under `alt_dir` as `edit` makes it of
/// its text, the empty text where the file is not there. `None` from
/// `edit` leaves the file as it is. Whether it was written.
pub fn rewrite_own<E: From<std::io::Error>>(
    alt_dir: &Path,
    edit: impl FnOnce(&[u8]) -> Result<Option<Vec<u8>>, E>,
) -> Result<bool, E> {
    let own = alt_dir.join(OWN);
    if let Some(parent) = own.parent() {
        fs::create_dir_all(parent)?;
    }
    let lock = own.with_extension("lock");
    let mut held = match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock)
    {
        Ok(held) => held,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let said = format!("could not lock config file {}: File exists", own.display());
            return Err(std::io::Error::new(e.kind(), said).into());
        }
        Err(e) => return Err(e.into()),
    };
    let write = || -> Result<bool, E> {
        let was = match fs::read(&own) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e.into()),
        };
        let Some(bytes) = edit(&was)? else {
            return Ok(false);
        };
        held.write_all(&bytes)?;
        held.sync_all()?;
        fs::rename(&lock, &own)?;
        Ok(true)
    };
    match write() {
        Ok(true) => Ok(true),
        Ok(false) => {
            fs::remove_file(&lock)?;
            Ok(false)
        }
        Err(failed) => {
            // the failure is what is told; the lock is not left to stand for it
            let _ = fs::remove_file(&lock);
            Err(failed)
        }
    }
}
