//! DEFLATE compression, in two kinds: a general compressor built for speed, and one
//! whose output is byte-identical with zlib.
//!
//! The reference for the second is zlib 1.3.2 with window bits 15, memLevel 8 and
//! the default strategy, compressing the whole input in one call into a buffer of
//! `deflateBound` / `compressBound` size.

mod bits;
mod block;
mod encoder;
mod quick;
mod tables;
mod trees;
mod window;
mod writer;

pub use writer::Writer;

/// Framing of the compressed stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// bare deflate stream
    Raw,
    /// two-byte zlib header, deflate stream, Adler-32 of the input
    Zlib,
    /// a gzip member without name or time, then CRC-32 and length of the input
    Gzip,
}

/// largest stored block
const MAX_STORED: usize = 65535;

/// Compresses `data` in one shot at `level` (0..=9), as fast as that level's
/// compression allows. The output is a valid stream but not the bytes zlib would
/// produce; [`compress_as_zlib`] is for that.
///
/// # Panics
///
/// If `level` is greater than 9.
pub fn compress(data: &[u8], level: u8, format: Format) -> Vec<u8> {
    wrap(data, level, format, quick::deflate)
}

/// Compresses `data` in one shot at `level` (0..=9) into exactly the bytes that zlib
/// produces for it with its default window, memory level and strategy.
///
/// # Panics
///
/// If `level` is greater than 9.
pub fn compress_as_zlib(data: &[u8], level: u8, format: Format) -> Vec<u8> {
    wrap(data, level, format, encoder::deflate)
}

fn wrap(
    data: &[u8],
    level: u8,
    format: Format,
    deflate: fn(&[u8], u8, Vec<u8>) -> Vec<u8>,
) -> Vec<u8> {
    assert!(level <= 9, "deflate level {level} out of range");
    let mut out = Vec::new();
    match format {
        Format::Raw => {}
        Format::Zlib => out.extend_from_slice(&zlib_header(level)),
        Format::Gzip => out.extend_from_slice(&gzip_header(level)),
    }
    let mut out = if level == 0 {
        store(data, out)
    } else {
        deflate(data, level, out)
    };
    match format {
        Format::Raw => {}
        Format::Zlib => out.extend_from_slice(&alt_hash::adler32(1, data).to_be_bytes()),
        Format::Gzip => {
            out.extend_from_slice(&alt_hash::crc32(data).to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        }
    }
    out
}

fn gzip_header(level: u8) -> [u8; 10] {
    // deflate, no optional fields, no time; then the effort hint and "unknown system"
    let effort = match level {
        9 => 2,
        1 => 4,
        _ => 0,
    };
    [0x1f, 0x8b, 8, 0, 0, 0, 0, 0, effort, 255]
}

fn zlib_header(level: u8) -> [u8; 2] {
    let level_flags: u16 = match level {
        0..=1 => 0,
        2..=5 => 1,
        6 => 2,
        _ => 3,
    };
    // deflate method, 32K window
    let header = 0x7800 | (level_flags << 6);
    (header + 31 - header % 31).to_be_bytes()
}

/// Level 0. With an output buffer of the bound size zlib copies straight
/// from the input, so every block but the last has the maximum length.
fn store(data: &[u8], mut out: Vec<u8>) -> Vec<u8> {
    out.reserve(data.len() + 5 * (data.len() / MAX_STORED + 1));
    let mut rest = data;
    loop {
        let (chunk, tail) = rest.split_at(rest.len().min(MAX_STORED));
        let len = chunk.len() as u16;
        out.push(u8::from(tail.is_empty()));
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(chunk);
        rest = tail;
        if rest.is_empty() {
            return out;
        }
    }
}
