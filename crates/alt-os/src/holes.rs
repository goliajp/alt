//! Where a file holds data and where it has holes.
//!
//! A file made longer without being written to, or one whose blocks never
//! reached the disk, has stretches the file system keeps no blocks for.
//! They read as zeros, and the file system tells where they are without
//! anyone reading them.

use std::{fs::File, io, os::fd::AsRawFd};

use crate::sys;

fn seek(file: &File, at: u64, origin: sys::c_int) -> io::Result<u64> {
    let Ok(at) = i64::try_from(at) else {
        return Err(io::Error::from_raw_os_error(sys::ENXIO));
    };
    // SAFETY: the descriptor is open for the duration of the borrow, and
    // lseek reads no memory
    let found = unsafe { sys::lseek(file.as_raw_fd(), at, origin) };
    match u64::try_from(found) {
        Ok(found) => Ok(found),
        Err(_) => Err(io::Error::last_os_error()),
    }
}

/// The first stretch of `file` at or after byte `from` that holds data:
/// where it starts and where the hole after it does, which is the end of
/// the file where no hole follows. `None` when nothing but a hole lies
/// between `from` and the end of the file.
///
/// A file system that does not tell holes from data is taken to hold data
/// all through. The place at which the descriptor is read next is left
/// wherever the search ended.
pub fn data_from(file: &File, from: u64) -> io::Result<Option<(u64, u64)>> {
    let untold = [sys::EINVAL, sys::ENOTSUP, sys::ENOTTY];
    let start = match seek(file, from, sys::SEEK_DATA) {
        Ok(start) => start,
        Err(e) if e.raw_os_error() == Some(sys::ENXIO) => return Ok(None),
        Err(e) if e.raw_os_error().is_some_and(|code| untold.contains(&code)) => {
            let len = file.metadata()?.len();
            return Ok((from < len).then_some((from, len)));
        }
        Err(e) => return Err(e),
    };
    Ok(Some((start, seek(file, start, sys::SEEK_HOLE)?)))
}
