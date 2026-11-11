//! Bit-level readers and the writer shared by the entropy stages.

use std::marker::PhantomData;

use crate::Error;

/// Reads 8 little-endian bytes at `p`.
///
/// # Safety
/// `p` must be valid for an 8-byte read.
#[inline(always)]
pub(crate) unsafe fn load64(p: *const u8) -> u64 {
    // safety: the caller guarantees 8 readable bytes
    u64::from_le(unsafe { p.cast::<u64>().read_unaligned() })
}

/// Reads 4 little-endian bytes at `p`.
///
/// # Safety
/// `p` must be valid for a 4-byte read.
#[inline(always)]
pub(crate) unsafe fn load32(p: *const u8) -> u32 {
    // safety: the caller guarantees 4 readable bytes
    u32::from_le(unsafe { p.cast::<u32>().read_unaligned() })
}

/// Reads a bitstream from its last byte towards its first, the direction in
/// which FSE and Huffman streams are consumed. Bits past the start read as
/// garbage and are reported by [`BackReader::overflowed`].
///
/// At most 56 bits may be read between two refills.
pub(crate) struct BackReader<'a> {
    /// start of the 8-byte window currently in `container`
    ptr: *const u8,
    start: *const u8,
    /// lowest window position from which a full refill is always in bounds
    fast: *const u8,
    container: u64,
    consumed: u32,
    life: PhantomData<&'a [u8]>,
}

impl<'a> BackReader<'a> {
    pub(crate) fn new(src: &'a [u8]) -> Result<Self, Error> {
        let last = *src.last().ok_or(Error::Corrupt)?;
        if last == 0 {
            return Err(Error::Corrupt);
        }
        // the highest set bit of the last byte only marks where the stream ends
        let pad = last.leading_zeros() + 1;
        match src.len().checked_sub(8) {
            Some(pos) => Ok(Self::resume(src, pos, pad)),
            None => {
                let mut bytes = [0u8; 8];
                bytes[..src.len()].copy_from_slice(src);
                Ok(Self {
                    ptr: src.as_ptr(),
                    start: src.as_ptr(),
                    fast: src.as_ptr().wrapping_add(8),
                    container: u64::from_le_bytes(bytes),
                    consumed: pad + 8 * (8 - src.len()) as u32,
                    life: PhantomData,
                })
            }
        }
    }

    /// Continues a stream of at least 8 bytes whose 8-byte window starts at
    /// `pos` and has `consumed` bits (at most 8) used.
    pub(crate) fn resume(src: &'a [u8], pos: usize, consumed: u32) -> Self {
        let window = &src[pos..pos + 8];
        Self {
            ptr: window.as_ptr(),
            start: src.as_ptr(),
            fast: src[8..].as_ptr(),
            container: u64::from_le_bytes(window.try_into().unwrap()),
            consumed,
            life: PhantomData,
        }
    }

    /// The next `n` bits (0..=56) without consuming them.
    #[inline(always)]
    pub(crate) fn peek(&self, n: u32) -> u64 {
        ((self.container << (self.consumed & 63)) >> 1) >> (63 - n)
    }

    #[inline(always)]
    pub(crate) fn skip(&mut self, n: u32) {
        self.consumed += n;
    }

    #[inline(always)]
    pub(crate) fn read(&mut self, n: u32) -> u64 {
        let bits = self.peek(n);
        self.consumed += n;
        bits
    }

    /// Slides the window down over the consumed bytes. Afterwards at least 57
    /// bits are available unless the stream is near its start.
    #[inline(always)]
    pub(crate) fn refill(&mut self) {
        if self.ptr >= self.fast {
            debug_assert!(self.consumed <= 64);
            // safety: the window starts at least 8 bytes into the stream and
            // at most 64 bits of it were consumed, so it moves down by at
            // most 8 bytes and stays inside the stream
            unsafe {
                self.ptr = self.ptr.sub((self.consumed >> 3) as usize);
                self.container = load64(self.ptr);
            }
            self.consumed &= 7;
        } else {
            self.refill_near_start();
        }
    }

    #[cold]
    fn refill_near_start(&mut self) {
        // safety: both pointers lie in the same stream, ptr at or above start
        let room = unsafe { self.ptr.offset_from(self.start) } as usize;
        let bytes = ((self.consumed >> 3) as usize).min(room);
        if bytes != 0 {
            self.consumed -= 8 * bytes as u32;
            // safety: room is nonzero only for streams of 8 bytes or more,
            // where every window from start to the initial one is in bounds
            unsafe {
                self.ptr = self.ptr.sub(bytes);
                self.container = load64(self.ptr);
            }
        }
    }

    /// True once more bits were read than the stream holds. Only meaningful
    /// right after [`BackReader::refill`].
    #[inline(always)]
    pub(crate) fn overflowed(&self) -> bool {
        self.consumed > 64
    }

    /// True when every bit was read and none beyond. Only meaningful right
    /// after [`BackReader::refill`].
    #[inline(always)]
    pub(crate) fn finished(&self) -> bool {
        self.ptr == self.start && self.consumed == 64
    }
}

/// Reads a bitstream from its first byte, least significant bit first. Bits
/// past the end read as zero; the caller compares [`FwdReader::bytes_used`]
/// with the input length.
pub(crate) struct FwdReader<'a> {
    src: &'a [u8],
    bit: usize,
}

impl<'a> FwdReader<'a> {
    pub(crate) fn new(src: &'a [u8]) -> Self {
        Self { src, bit: 0 }
    }

    /// The next 32 bits without consuming them.
    pub(crate) fn peek(&self) -> u32 {
        let at = (self.bit >> 3).min(self.src.len());
        let rest = &self.src[at..];
        let mut bytes = [0u8; 8];
        let n = rest.len().min(8);
        bytes[..n].copy_from_slice(&rest[..n]);
        (u64::from_le_bytes(bytes) >> (self.bit & 7)) as u32
    }

    pub(crate) fn skip(&mut self, n: u32) {
        self.bit += n as usize;
    }

    pub(crate) fn bytes_used(&self) -> usize {
        self.bit.div_ceil(8)
    }
}

/// Appends bits to a byte vector, least significant bit first.
pub(crate) struct BitWriter<'a> {
    out: &'a mut Vec<u8>,
    acc: u64,
    bits: u32,
}

impl<'a> BitWriter<'a> {
    pub(crate) fn new(out: &'a mut Vec<u8>) -> Self {
        Self {
            out,
            acc: 0,
            bits: 0,
        }
    }

    /// Adds the low `n` bits of `value`, which must have no higher bits set.
    /// At most 56 bits may be added between two flushes.
    #[inline(always)]
    pub(crate) fn put(&mut self, value: u64, n: u32) {
        debug_assert!(value >> n == 0 && self.bits + n <= 63);
        self.acc |= value << self.bits;
        self.bits += n;
    }

    /// Moves every whole byte to the output; fewer than 8 bits stay pending.
    #[inline(always)]
    pub(crate) fn flush(&mut self) {
        if self.out.capacity() - self.out.len() < 8 {
            self.out.reserve(512);
        }
        let len = self.out.len();
        // safety: 8 bytes of spare capacity exist past len, all 8 are written,
        // and the length grows only over the whole bytes among them
        unsafe {
            let dst = self.out.as_mut_ptr().add(len);
            dst.cast::<u64>().write_unaligned(self.acc.to_le());
            self.out.set_len(len + (self.bits >> 3) as usize);
        }
        self.acc >>= self.bits & !7;
        self.bits &= 7;
    }

    /// Ends a stream that is read forwards.
    pub(crate) fn finish(mut self) {
        self.flush();
        if self.bits != 0 {
            self.out.push(self.acc as u8);
        }
    }

    /// Ends a stream that is read backwards: a final set bit tells the reader
    /// where the data stops.
    pub(crate) fn finish_marked(mut self) {
        self.put(1, 1);
        self.finish();
    }
}
