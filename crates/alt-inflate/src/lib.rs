//! DEFLATE, zlib and gzip decompression.
//!
//! [`Inflater`] decodes a stream in as many calls as the caller likes; the functions
//! below it cover the common case of a whole stream in one buffer.

mod blocks;
mod fast;
mod huffman;
mod inflater;
mod reader;
mod wrapper;

pub use inflater::{Inflater, Step};
pub use reader::Reader;

/// what surrounds the DEFLATE data
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// nothing
    Raw,
    /// two header bytes and an Adler-32 of the content
    Zlib,
    /// a gzip member: header, then CRC-32 and length of the content
    Gzip,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// the stream is not valid; the text says what was wrong
    Corrupt(&'static str),
    /// the content does not match the checksum or length stored after it
    Checksum,
    /// the stream ended before its end marker
    Truncated,
    /// the content is longer than the caller allows
    TooLong,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Corrupt(what) => write!(f, "corrupt compressed data: {what}"),
            Self::Checksum => f.write_str("compressed data fails its checksum"),
            Self::Truncated => f.write_str("compressed data ends early"),
            Self::TooLong => f.write_str("decompressed data exceeds the limit"),
        }
    }
}

impl std::error::Error for Error {}

thread_local! {
    /// The decoder the functions below reuse: its tables are larger than most of the
    /// streams they are called for.
    static SCRATCH: std::cell::RefCell<Inflater> =
        std::cell::RefCell::new(Inflater::new(Format::Raw));
}

/// Decompresses one stream at the start of `input` into exactly `output`, and returns
/// how many input bytes the stream occupied. A stream whose content has another length
/// is an error.
pub fn inflate_exact(format: Format, input: &[u8], output: &mut [u8]) -> Result<usize, Error> {
    SCRATCH.with_borrow_mut(|inflater| {
        inflater.restart(format);
        exact(inflater, input, output)
    })
}

fn exact(inflater: &mut Inflater, input: &[u8], output: &mut [u8]) -> Result<usize, Error> {
    let step = inflater.inflate(input, output)?;
    if step.done && step.produced == output.len() {
        return Ok(step.consumed);
    }
    if step.produced < output.len() {
        return Err(Error::Truncated);
    }
    // the output is full: the stream must end without producing anything more
    let mut probe = [0u8; 1];
    let rest = inflater.inflate(&input[step.consumed..], &mut probe)?;
    match (rest.done, rest.produced) {
        (true, 0) => Ok(step.consumed + rest.consumed),
        (_, 0) => Err(Error::Truncated),
        _ => Err(Error::TooLong),
    }
}

/// Decompresses one stream at the start of `input`, whose content may be at most
/// `limit` bytes, and returns the content with the number of input bytes it occupied.
pub fn inflate(format: Format, input: &[u8], limit: usize) -> Result<(Vec<u8>, usize), Error> {
    SCRATCH.with_borrow_mut(|inflater| {
        inflater.restart(format);
        growing(inflater, input, limit)
    })
}

fn growing(inflater: &mut Inflater, input: &[u8], limit: usize) -> Result<(Vec<u8>, usize), Error> {
    let mut output = Vec::new();
    let mut consumed = 0;
    // most streams expand a few times; grow from there
    let mut room = input.len().saturating_mul(4).clamp(256, 1 << 20);
    loop {
        let filled = output.len();
        let room_now = room.min(limit.saturating_add(1) - filled);
        output.resize(filled + room_now, 0);
        let step = inflater.inflate(&input[consumed..], &mut output[filled..])?;
        consumed += step.consumed;
        output.truncate(filled + step.produced);
        if output.len() > limit {
            return Err(Error::TooLong);
        }
        if step.done {
            return Ok((output, consumed));
        }
        if step.produced < room_now {
            return Err(Error::Truncated);
        }
        room = (room * 2).min(64 << 20);
    }
}
