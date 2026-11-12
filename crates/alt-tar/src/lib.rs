//! Streaming tar archives.
//!
//! [`Builder`] writes GNU-format archives of regular files, using a GNU long
//! name member for paths over 100 bytes. [`Archive`] reads ustar, PAX and GNU
//! archives one member at a time. Neither holds more than one buffer of file
//! data in memory.
//!
//! Reading treats the archive as untrusted: a member whose path is absolute or
//! contains a `..` component, a header with a bad checksum or a malformed
//! number, an oversized extension record and an archive that ends inside a
//! member are all errors.
#![forbid(unsafe_code)]

mod header;
mod pax;
mod read;
mod write;

pub use read::{Archive, Entry, Kind};
pub use write::Builder;

use std::{fmt, io};

/// The longest member path accepted when reading.
pub const MAX_PATH: usize = 4096;
/// The largest long-name or PAX extension record accepted when reading.
pub const MAX_EXTENSION: u64 = 1024 * 1024;

// the capacity of a linux pipe; larger copies through a pipe measured slower
const BUFFER: usize = 64 * 1024;

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    /// The archive ends inside a header, a member or its padding.
    Truncated,
    Checksum,
    /// A numeric field is malformed or does not fit in 64 bits.
    Number,
    /// A member path is empty, absolute, or contains `..` or a NUL byte.
    Path,
    /// A path or extension record exceeds [`MAX_PATH`] or [`MAX_EXTENSION`].
    TooLong,
    /// A PAX extension record is malformed, or two extensions describe one member.
    Extension,
    /// A sparse member.
    Unsupported,
    /// The data given to [`Builder::append_file`] is not as long as declared.
    Size,
}

pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(error) => write!(f, "tar i/o: {error}"),
            Error::Truncated => f.write_str("tar archive is truncated"),
            Error::Checksum => f.write_str("tar header checksum mismatch"),
            Error::Number => f.write_str("tar numeric field is malformed or too large"),
            Error::Path => f.write_str("tar member path is empty, absolute or escapes the root"),
            Error::TooLong => f.write_str("tar path or extension record is too long"),
            Error::Extension => f.write_str("tar extension record is malformed or repeated"),
            Error::Unsupported => f.write_str("tar sparse members are not supported"),
            Error::Size => f.write_str("tar member data does not match its declared size"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Error::Io(error)
    }
}

fn read_some<R: io::Read + ?Sized>(reader: &mut R, buffer: &mut [u8]) -> io::Result<usize> {
    loop {
        match reader.read(buffer) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            other => return other,
        }
    }
}
