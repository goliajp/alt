//! Frame and block level decoding.

use std::cell::Cell;

use crate::Error;
use crate::execute::{OUT_SLACK, Sink};
use crate::literals::LiteralDecoder;
use crate::sequences::SequenceDecoder;
use crate::xxh64::xxh64;

pub(crate) const MAGIC: u32 = 0xFD2F_B528;
const SKIPPABLE_MAGIC: u32 = 0x184D_2A50;
pub(crate) const BLOCK_MAX: usize = 128 << 10;

struct Decoder {
    literals: LiteralDecoder,
    sequences: SequenceDecoder,
    repeats: [usize; 3],
}

struct Header {
    /// declared size of the content, when the frame states it
    content_size: Option<u64>,
    block_max: usize,
    checksum: bool,
    len: usize,
}

fn take<'a>(src: &mut &'a [u8], len: usize) -> Result<&'a [u8], Error> {
    let (head, rest) = src.split_at_checked(len).ok_or(Error::Truncated)?;
    *src = rest;
    Ok(head)
}

fn le(bytes: &[u8]) -> u64 {
    let mut wide = [0u8; 8];
    wide[..bytes.len()].copy_from_slice(bytes);
    u64::from_le_bytes(wide)
}

fn read_header(src: &[u8]) -> Result<Header, Error> {
    let mut rest = src;
    let descriptor = take(&mut rest, 1)?[0];
    if descriptor & 0x08 != 0 {
        return Err(Error::Unsupported);
    }
    let single_segment = descriptor & 0x20 != 0;
    let mut window = 0u64;
    if !single_segment {
        let byte = take(&mut rest, 1)?[0] as u64;
        let base = 1u64 << (10 + (byte >> 3));
        window = base + (base >> 3) * (byte & 7);
    }
    let dictionary = le(take(&mut rest, [0, 1, 2, 4][(descriptor & 3) as usize])?);
    if dictionary != 0 {
        return Err(Error::Unsupported);
    }
    let content_size = match descriptor >> 6 {
        0 if single_segment => Some(le(take(&mut rest, 1)?)),
        0 => None,
        1 => Some(le(take(&mut rest, 2)?) + 256),
        2 => Some(le(take(&mut rest, 4)?)),
        _ => Some(le(take(&mut rest, 8)?)),
    };
    if single_segment {
        window = content_size.unwrap_or(0);
    }
    Ok(Header {
        content_size,
        block_max: window.min(BLOCK_MAX as u64) as usize,
        checksum: descriptor & 0x04 != 0,
        len: src.len() - rest.len(),
    })
}

impl Decoder {
    fn new() -> Self {
        Self {
            literals: LiteralDecoder::new(),
            sequences: SequenceDecoder::new(),
            repeats: [1, 4, 8],
        }
    }

    fn compressed_block(
        &mut self,
        block: &[u8],
        out: &mut Vec<u8>,
        frame_start: usize,
        prefix: &[u8],
        block_max: usize,
        room: usize,
    ) -> Result<(), Error> {
        let (literals, used) = self.literals.decode(block, block_max)?;
        let rest = &block[used..];
        let first = *rest.first().ok_or(Error::Corrupt)? as usize;
        let (count, header) = match first {
            0..128 => (first, 1),
            128..255 => {
                let low = *rest.get(1).ok_or(Error::Corrupt)? as usize;
                (((first - 128) << 8) + low, 2)
            }
            _ => {
                let low = rest.get(1..3).ok_or(Error::Corrupt)?;
                (le(low) as usize + 0x7F00, 3)
            }
        };
        if count == 0 {
            if header != rest.len() {
                return Err(Error::Corrupt);
            }
            // safety: the literal decoder returns len initialized bytes
            let bytes = unsafe { std::slice::from_raw_parts(literals.ptr, literals.len) };
            out.extend_from_slice(bytes);
            return Ok(());
        }
        let tables = self.sequences.read_tables(&rest[header..])?;
        // a block may fill what the frame or the caller still allows, which
        // also keeps the buffer from growing past what was reserved for it
        let sink = Sink {
            out,
            frame_start,
            prefix,
            max_len: block_max.min(room),
            overflow: if room < block_max {
                Error::SizeMismatch
            } else {
                Error::Corrupt
            },
        };
        self.sequences.execute(
            &rest[header + tables..],
            count,
            literals,
            sink,
            &mut self.repeats,
        )
    }

    /// Decodes one frame (after its magic number) and returns the input left
    /// after it.
    fn frame<'a>(
        &mut self,
        src: &'a [u8],
        out: &mut Vec<u8>,
        prefix: &[u8],
        limit: usize,
        reserve: usize,
    ) -> Result<&'a [u8], Error> {
        let header = read_header(src)?;
        let mut rest = &src[header.len..];
        let frame_start = out.len();
        let over = |len: usize| {
            len > limit
                || header
                    .content_size
                    .is_some_and(|size| (len - frame_start) as u64 > size)
        };
        if let Some(size) = header.content_size {
            if over(frame_start.saturating_add(size.min(usize::MAX as u64) as usize)) {
                return Err(Error::SizeMismatch);
            }
            // a hint only: the declared size is not trusted beyond `reserve`
            out.reserve(size.min(reserve as u64) as usize + OUT_SLACK);
        }
        self.literals.reset();
        self.sequences.reset();
        self.repeats = [1, 4, 8];
        loop {
            let block = le(take(&mut rest, 3)?) as usize;
            let len = block >> 3;
            if len > header.block_max {
                return Err(Error::Corrupt);
            }
            match (block >> 1) & 3 {
                0 => {
                    if over(out.len() + len) {
                        return Err(Error::SizeMismatch);
                    }
                    out.extend_from_slice(take(&mut rest, len)?);
                }
                1 => {
                    if over(out.len() + len) {
                        return Err(Error::SizeMismatch);
                    }
                    let byte = take(&mut rest, 1)?[0];
                    out.resize(out.len() + len, byte);
                }
                2 => {
                    let body = take(&mut rest, len)?;
                    let declared = header
                        .content_size
                        .map_or(u64::MAX, |size| size - (out.len() - frame_start) as u64);
                    let room = declared.min((limit - out.len()) as u64);
                    let room = room.min(usize::MAX as u64) as usize;
                    self.compressed_block(body, out, frame_start, prefix, header.block_max, room)?;
                    if over(out.len()) {
                        return Err(Error::SizeMismatch);
                    }
                }
                _ => return Err(Error::Corrupt),
            }
            if block & 1 != 0 {
                break;
            }
        }
        let content = &out[frame_start..];
        if header
            .content_size
            .is_some_and(|size| content.len() as u64 != size)
        {
            return Err(Error::SizeMismatch);
        }
        if header.checksum {
            let stored = le(take(&mut rest, 4)?) as u32;
            if xxh64(content, 0) as u32 != stored {
                return Err(Error::Checksum);
            }
        }
        Ok(rest)
    }
}

/// Decodes every frame in `src`. `prefix` is history placed before the
/// first data frame; the output may not grow past `limit`, which the caller
/// vouches for when it is not `usize::MAX`.
pub(crate) fn decode(src: &[u8], prefix: &[u8], limit: usize) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    // what a frame may make us allocate before it has produced anything:
    // the caller's length if there is one, else a generous multiple of the
    // input, so that a forged content size cannot ask for the moon
    let reserve = if limit == usize::MAX {
        (src.len() << 5) + (1 << 20)
    } else {
        let wanted = limit.checked_add(OUT_SLACK).ok_or(Error::SizeMismatch)?;
        out.try_reserve_exact(wanted)
            .map_err(|_| Error::SizeMismatch)?;
        limit
    };
    if src.is_empty() {
        return Err(Error::Truncated);
    }
    let mut decoder = KEPT.take().unwrap_or_else(|| Box::new(Decoder::new()));
    let done = decoder.frames(src, &mut out, prefix, limit, reserve);
    KEPT.set(Some(decoder));
    done.map(|()| out)
}

impl Decoder {
    /// Decodes every frame in `src`, appending to `out`.
    fn frames(
        &mut self,
        src: &[u8],
        out: &mut Vec<u8>,
        mut prefix: &[u8],
        limit: usize,
        reserve: usize,
    ) -> Result<(), Error> {
        let mut rest = src;
        while !rest.is_empty() {
            let magic = le(take(&mut rest, 4)?) as u32;
            if magic & !0xF == SKIPPABLE_MAGIC {
                let len = le(take(&mut rest, 4)?) as usize;
                take(&mut rest, len)?;
                continue;
            }
            if magic != MAGIC {
                return Err(Error::BadMagic);
            }
            rest = self.frame(rest, out, prefix, limit, reserve)?;
            prefix = &[];
        }
        Ok(())
    }
}

thread_local! {
    /// Tables and the literal buffer of the last decode on this thread,
    /// kept so that decoding many small inputs does not set them up anew
    /// for each. A frame resets what it needs.
    static KEPT: Cell<Option<Box<Decoder>>> = const { Cell::new(None) };
}
