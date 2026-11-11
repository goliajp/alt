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
    /// bits of the window not read yet; negative once the stream overflowed
    left: i32,
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
                    left: 8 * src.len() as i32 - pad as i32,
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
            left: 64 - consumed as i32,
            life: PhantomData,
        }
    }

    /// The next `n` bits (1..=56) without consuming them; where the stream
    /// has fewer left, zeros stand in for the missing low bits.
    #[inline(always)]
    pub(crate) fn peek_padded(&self, n: u32) -> u64 {
        (self.container << ((64 - self.left) as u32 & 63)) >> (64 - n)
    }

    #[inline(always)]
    pub(crate) fn skip(&mut self, n: u32) {
        self.left -= n as i32;
    }

    /// Consumes and returns the next `n` bits (0..=56). Past the start of
    /// the stream the result is arbitrary.
    #[inline(always)]
    pub(crate) fn read(&mut self, n: u32) -> u64 {
        self.left -= n as i32;
        (self.container >> (self.left as u32 & 63)) & ((1 << n) - 1)
    }

    /// Slides the window down over the consumed bytes. Afterwards at least 57
    /// bits are available unless the stream is near its start.
    #[inline(always)]
    pub(crate) fn refill(&mut self) {
        if self.ptr >= self.fast {
            debug_assert!((0..=64).contains(&self.left));
            let used = (64 - self.left) as u32;
            // safety: the window starts at least 8 bytes into the stream and
            // at most 64 bits of it were consumed, so it moves down by at
            // most 8 bytes and stays inside the stream
            unsafe {
                self.ptr = self.ptr.sub((used >> 3) as usize);
                self.container = load64(self.ptr);
            }
            self.left += (used & !7) as i32;
        } else {
            self.refill_near_start();
        }
    }

    #[cold]
    fn refill_near_start(&mut self) {
        // safety: both pointers lie in the same stream, ptr at or above start
        let room = unsafe { self.ptr.offset_from(self.start) } as usize;
        let bytes = (((64 - self.left) >> 3) as usize).min(room);
        if bytes != 0 {
            self.left += 8 * bytes as i32;
            // safety: room is nonzero only for streams of 8 bytes or more,
            // where every window from start to the initial one is in bounds
            unsafe {
                self.ptr = self.ptr.sub(bytes);
                self.container = load64(self.ptr);
            }
        }
    }

    /// True while refills take whole bytes without regard to the start of
    /// the stream. Right after a refill it tells that the refill was such a
    /// one, so that at least 57 bits can be read before the next.
    #[inline(always)]
    pub(crate) fn roomy(&self) -> bool {
        self.ptr >= self.fast
    }

    /// The read position within the window, for [`BackReader::rewind`].
    #[inline(always)]
    pub(crate) fn mark(&self) -> i32 {
        self.left
    }

    /// Takes back every read since `mark` was taken; no refill may have
    /// happened in between.
    #[inline(always)]
    pub(crate) fn rewind(&mut self, mark: i32) {
        self.left = mark;
    }

    /// True once more bits were read than the stream holds. Only meaningful
    /// right after [`BackReader::refill`].
    #[inline(always)]
    pub(crate) fn overflowed(&self) -> bool {
        self.left < 0
    }

    /// True when every bit was read and none beyond. Only meaningful right
    /// after [`BackReader::refill`].
    #[inline(always)]
    pub(crate) fn finished(&self) -> bool {
        self.ptr == self.start && self.left == 0
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
    #[inline(always)]
    pub(crate) fn peek(&self) -> u32 {
        let at = self.bit >> 3;
        let word = match self.src.get(at..at + 8) {
            Some(bytes) => u64::from_le_bytes(bytes.try_into().unwrap()),
            None => self.peek_near_end(at),
        };
        (word >> (self.bit & 7)) as u32
    }

    #[cold]
    fn peek_near_end(&self, at: usize) -> u64 {
        let rest = &self.src[at.min(self.src.len())..];
        let mut bytes = [0u8; 8];
        let n = rest.len().min(8);
        bytes[..n].copy_from_slice(&rest[..n]);
        u64::from_le_bytes(bytes)
    }

    #[inline(always)]
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

/// A [`BitWriter`] without capacity checks, for streams whose size is known
/// beforehand. Bits go to raw memory; the caller accounts for the length.
pub(crate) struct RawBitWriter {
    at: *mut u8,
    acc: u64,
    bits: u32,
}

impl RawBitWriter {
    /// Starts writing at `at`.
    ///
    /// # Safety
    /// `at` must be valid for writes of every byte the stream will hold
    /// plus 8.
    pub(crate) unsafe fn new(at: *mut u8) -> Self {
        Self {
            at,
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
        // safety: the constructor's caller reserved 8 bytes past the stream
        unsafe {
            self.at.cast::<u64>().write_unaligned(self.acc.to_le());
            self.at = self.at.add((self.bits >> 3) as usize);
        }
        self.acc >>= self.bits & !7;
        self.bits &= 7;
    }

    /// Ends a stream that is read backwards with its marker bit and returns
    /// the address one past its last byte.
    pub(crate) fn finish_marked(mut self) -> *mut u8 {
        self.put(1, 1);
        self.flush();
        // safety: as for flush; the pending bits were stored by it already
        unsafe { self.at.add((self.bits != 0) as usize) }
    }
}
