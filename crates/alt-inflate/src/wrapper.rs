//! the headers of the zlib and gzip formats

use crate::Error;

/// Checks a zlib header at the start of `input`. None when fewer than its two bytes
/// are there yet.
pub(crate) fn zlib_header(input: &[u8]) -> Result<Option<usize>, Error> {
    let Some([method, flags]) = input.first_chunk::<2>() else {
        return Ok(None);
    };
    if method & 0x0f != 8 || method >> 4 > 7 {
        return Err(Error::Corrupt("unknown compression method"));
    }
    if (u16::from(*method) << 8 | u16::from(*flags)) % 31 != 0 {
        return Err(Error::Corrupt("incorrect header check"));
    }
    if flags & 0x20 != 0 {
        return Err(Error::Corrupt("a preset dictionary is required"));
    }
    Ok(Some(2))
}

/// Checks a gzip member header at the start of `input` and returns its length. None
/// when the header is not complete yet.
pub(crate) fn gzip_header(input: &[u8]) -> Result<Option<usize>, Error> {
    const HEADER_CRC: u8 = 2;
    const EXTRA: u8 = 4;
    const NAME: u8 = 8;
    const COMMENT: u8 = 16;
    let Some(fixed) = input.first_chunk::<10>() else {
        return Ok(None);
    };
    if fixed[0] != 0x1f || fixed[1] != 0x8b {
        return Err(Error::Corrupt("not a gzip stream"));
    }
    if fixed[2] != 8 {
        return Err(Error::Corrupt("unknown compression method"));
    }
    let flags = fixed[3];
    if flags & 0xe0 != 0 {
        return Err(Error::Corrupt("unknown header flags"));
    }
    let mut at = 10;
    if flags & EXTRA != 0 {
        let Some(length) = input.get(at..at + 2) else {
            return Ok(None);
        };
        at += 2 + usize::from(u16::from_le_bytes([length[0], length[1]]));
    }
    for text in [NAME, COMMENT] {
        if flags & text != 0 {
            let Some(end) = input
                .get(at..)
                .and_then(|rest| rest.iter().position(|b| *b == 0))
            else {
                return Ok(None);
            };
            at += end + 1;
        }
    }
    if flags & HEADER_CRC != 0 {
        let Some(stored) = input.get(at..at + 2) else {
            return Ok(None);
        };
        let computed = alt_hash::crc32(&input[..at]) as u16;
        if u16::from_le_bytes([stored[0], stored[1]]) != computed {
            return Err(Error::Corrupt("header fails its checksum"));
        }
        at += 2;
    }
    if input.len() < at {
        return Ok(None);
    }
    Ok(Some(at))
}
