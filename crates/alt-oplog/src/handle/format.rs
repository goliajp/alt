//! Which format the log in a directory is of, by its first bytes.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use crate::OpLogError;
use crate::v2::block::VERSION as V2;

/// The first bytes of the log in `dir`, which tell its format: `None`
/// for a log that is not there yet or holds no whole header of either.
fn header_of(dir: &Path) -> std::io::Result<Option<[u8; 5]>> {
    let mut header = [0u8; 5];
    let read = File::open(dir.join("log")).and_then(|mut file| file.read_exact(&mut header));
    match read {
        Ok(()) => Ok(Some(header)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Ok(None),
        Err(e) => Err(e),
    }
}

/// The version the header of the log in `dir` gives: `None` for a log
/// that is not there yet or holds no whole header of either format.
pub(crate) fn format_of(dir: &Path) -> std::io::Result<Option<u8>> {
    Ok(header_of(dir)?.map(|header| header[4]))
}

/// [`format_of`] for an open, which refuses a file that is no log of a
/// format this build reads on its first bytes: a version 1 log is read
/// whole, and no other file is read so for being taken for one.
pub(super) fn format_to_open(dir: &Path) -> Result<Option<u8>, OpLogError> {
    let header = header_of(dir)?;
    if let Some(header) = header.filter(|header| header[4] != V2) {
        crate::scan::check_header(&header)?;
    }
    Ok(header.map(|header| header[4]))
}

/// Whether a log this process creates is one of version 2: by
/// `ALT_OPLOG_FORMAT=2` in its environment.
pub(super) fn creates_v2() -> bool {
    std::env::var_os("ALT_OPLOG_FORMAT").is_some_and(|format| format == "2")
}
