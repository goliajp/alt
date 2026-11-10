//! One sequential pass over a pack: entry boundaries, inflated payloads, entry checksums.

use crate::{Error, Kind, Oid};

#[derive(Debug, Clone, Copy)]
pub(crate) enum Form {
    Plain(Kind),
    /// as parsed: distance back to the base entry; `scan` turns it into the base's offset
    Offset(u64),
    Reference(Oid),
}

pub(crate) struct Raw {
    pub offset: u64,
    pub len: u32,
    pub header: u8,
    pub crc: u32,
    pub form: Form,
    /// inflated payload: object bytes for a plain entry, delta instructions otherwise
    pub size: u64,
    pub data: Option<Vec<u8>>,
}

/// kind bits, inflated size, form and header length of the entry at `pos`
pub(crate) fn header(pack: &[u8], pos: usize) -> Result<(Form, u64, usize), Error> {
    let fail = || Error::Format("entry header");
    let mut at = pos;
    let mut byte = *pack.get(at).ok_or_else(fail)?;
    at += 1;
    let code = byte >> 4 & 7;
    let mut size = u64::from(byte & 15);
    let mut shift = 4;
    while byte & 0x80 != 0 {
        byte = *pack.get(at).ok_or_else(fail)?;
        at += 1;
        if shift > 57 {
            return Err(fail());
        }
        size |= u64::from(byte & 0x7f) << shift;
        shift += 7;
    }
    let form = match code {
        1..=4 => Form::Plain(Kind::from_code(code).unwrap()),
        6 => {
            let mut byte = *pack.get(at).ok_or_else(fail)?;
            at += 1;
            let mut distance = u64::from(byte & 0x7f);
            while byte & 0x80 != 0 {
                byte = *pack.get(at).ok_or_else(fail)?;
                at += 1;
                distance = distance
                    .checked_add(1)
                    .and_then(|value| value.checked_mul(128))
                    .ok_or_else(fail)?
                    | u64::from(byte & 0x7f);
            }
            if distance == 0 {
                return Err(fail());
            }
            Form::Offset(distance)
        }
        7 => {
            let oid = pack.get(at..at + 20).ok_or_else(fail)?;
            at += 20;
            Form::Reference(oid.try_into().unwrap())
        }
        _ => return Err(fail()),
    };
    Ok((form, size, at - pos))
}

/// inflates exactly `size` bytes from `input`; returns the bytes and the compressed length
pub(crate) fn inflate(input: &[u8], size: usize) -> Result<(Vec<u8>, usize), Error> {
    let mut out = vec![0; size];
    let consumed = alt_inflate::inflate_exact(alt_inflate::Format::Zlib, input, &mut out).map_err(
        |error| {
            Error::Format(match error {
                alt_inflate::Error::TooLong => "entry larger than its header",
                alt_inflate::Error::TooShort => "entry shorter than its header",
                alt_inflate::Error::Truncated => "truncated zlib stream",
                _ => "zlib stream",
            })
        },
    )?;
    Ok((out, consumed))
}

pub(crate) struct Scanned {
    pub entries: Vec<Raw>,
    pub body_end: usize,
}

/// `keep` bounds which plain payloads stay in memory; larger ones are handed to `large`
/// (which hashes them) and dropped
pub(crate) fn scan(
    pack: &[u8],
    limits: &crate::Limits,
    keep: usize,
    mut large: impl FnMut(usize, Kind, &[u8]),
) -> Result<Scanned, Error> {
    if pack.len() < 32 || &pack[..4] != b"PACK" {
        return Err(Error::Format("not a pack"));
    }
    if u32::from_be_bytes(pack[4..8].try_into().unwrap()) != 2 {
        return Err(Error::Format("pack version"));
    }
    let count = u32::from_be_bytes(pack[8..12].try_into().unwrap()) as usize;
    if count as u64 > limits.objects {
        return Err(Error::Limit("objects"));
    }
    let body_end = pack.len() - 20;
    let mut entries = Vec::with_capacity(count);
    let mut inflated = 0u64;
    let mut pos = 12;
    for index in 0..count {
        let (form, size, head) = header(&pack[..body_end], pos)?;
        let form = match form {
            Form::Offset(distance) => Form::Offset(
                (pos as u64)
                    .checked_sub(distance)
                    .filter(|base| *base >= 12)
                    .ok_or(Error::Format("delta base offset"))?,
            ),
            other => other,
        };
        if size > limits.object_bytes {
            return Err(Error::Limit("object bytes"));
        }
        inflated = inflated
            .checked_add(size)
            .filter(|total| *total <= limits.inflated_bytes)
            .ok_or(Error::Limit("inflated bytes"))?;
        let (data, compressed) = inflate(&pack[pos + head..body_end], size as usize)?;
        let len = head + compressed;
        let data = match form {
            Form::Plain(kind @ Kind::Blob) if data.len() > keep => {
                large(index, kind, &data);
                None
            }
            _ => Some(data),
        };
        entries.push(Raw {
            offset: pos as u64,
            len: u32::try_from(len).map_err(|_| Error::Limit("entry bytes"))?,
            header: head as u8,
            crc: crate::format::crc32(&pack[pos..pos + len]),
            form,
            size,
            data,
        });
        pos += len;
    }
    if pos != body_end {
        return Err(Error::Format("bytes after the last entry"));
    }
    Ok(Scanned { entries, body_end })
}
