//! The literals section of a compressed block.

use crate::Error;
use crate::huffman::HufTable;

/// Bytes that may be read past the end of a literal run by wide copies.
pub(crate) const LIT_SLACK: usize = 32;

/// Decoded literals of one block: `len` initialized bytes at `ptr`, followed
/// by at least `LIT_SLACK` readable bytes.
#[derive(Clone, Copy)]
pub(crate) struct Literals {
    pub ptr: *const u8,
    pub len: usize,
}

/// Huffman state that outlives a block, plus the buffer literals decode into.
pub(crate) struct LiteralDecoder {
    table: HufTable,
    table_valid: bool,
    buf: Vec<u8>,
}

impl LiteralDecoder {
    pub(crate) fn new() -> Self {
        Self {
            table: HufTable::new(),
            table_valid: false,
            buf: Vec::new(),
        }
    }

    pub(crate) fn reset(&mut self) {
        self.table_valid = false;
    }

    /// Bytes held by the buffer literals decode into.
    #[cfg(test)]
    pub(crate) fn capacity(&self) -> usize {
        self.buf.capacity()
    }

    /// Start of a scratch area with room for `len + LIT_SLACK` bytes.
    fn scratch(&mut self, len: usize) -> *mut u8 {
        if self.buf.capacity() < len + LIT_SLACK {
            self.buf = Vec::with_capacity(len + LIT_SLACK);
        }
        self.buf.as_mut_ptr()
    }

    /// Decodes the literals section at the start of `block`. Returns the
    /// literals and the size of the section. The literals stay valid until
    /// the next call and for as long as `block` is borrowed.
    pub(crate) fn decode(
        &mut self,
        block: &[u8],
        max_len: usize,
    ) -> Result<(Literals, usize), Error> {
        let first = *block.first().ok_or(Error::Corrupt)? as usize;
        let field = |bytes: usize| -> Result<usize, Error> {
            let raw = block.get(..bytes).ok_or(Error::Corrupt)?;
            let mut wide = [0u8; 8];
            wide[..bytes].copy_from_slice(raw);
            Ok(u64::from_le_bytes(wide) as usize)
        };
        let kind = first & 3;
        let format = (first >> 2) & 3;
        if kind < 2 {
            let (header, len) = match format {
                1 => (2, field(2)? >> 4),
                3 => (3, field(3)? >> 4),
                _ => (1, first >> 3),
            };
            if len > max_len {
                return Err(Error::Corrupt);
            }
            if kind == 0 {
                let end = header + len;
                let raw = block.get(header..end).ok_or(Error::Corrupt)?;
                if block.len() - end >= LIT_SLACK {
                    return Ok((
                        Literals {
                            ptr: raw.as_ptr(),
                            len,
                        },
                        end,
                    ));
                }
                let ptr = self.scratch(len);
                // safety: the scratch area holds at least len bytes
                unsafe { ptr.copy_from_nonoverlapping(raw.as_ptr(), len) };
                return Ok((Literals { ptr, len }, end));
            }
            let byte = *block.get(header).ok_or(Error::Corrupt)?;
            let ptr = self.scratch(len);
            // safety: the scratch area holds len + LIT_SLACK bytes
            unsafe { ptr.write_bytes(byte, len + LIT_SLACK) };
            return Ok((Literals { ptr, len }, header + 1));
        }

        let (header, len, packed, four) = match format {
            0 | 1 => {
                let bits = field(3)?;
                (3, (bits >> 4) & 0x3FF, bits >> 14, format == 1)
            }
            2 => {
                let bits = field(4)?;
                (4, (bits >> 4) & 0x3FFF, bits >> 18, true)
            }
            _ => {
                let bits = field(5)?;
                (5, (bits >> 4) & 0x3_FFFF, bits >> 22, true)
            }
        };
        if len == 0 || len > max_len {
            return Err(Error::Corrupt);
        }
        let mut body = block.get(header..header + packed).ok_or(Error::Corrupt)?;
        if kind == 2 {
            let used = self.table.read(body)?;
            self.table_valid = true;
            body = &body[used..];
        } else if !self.table_valid {
            return Err(Error::Corrupt);
        }
        let ptr = self.scratch(len);
        // safety: the scratch area holds at least len bytes
        unsafe {
            if four {
                self.table.decode_4x(body, ptr, len)?;
            } else {
                self.table.decode_stream(body, ptr, len)?;
            }
        }
        Ok((Literals { ptr, len }, header + packed))
    }
}
