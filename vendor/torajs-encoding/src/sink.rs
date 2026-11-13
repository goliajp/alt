//! The decoders' output: UTF-8 appended to a `Vec<u8>` whose length is
//! kept in a local and written back once, on drop.

use alloc::vec::Vec;

use crate::{DecodeError, REPLACEMENT};

/// The high bit of each byte of a word.
pub(crate) const HIGH: u64 = 0x8080_8080_8080_8080;

/// ASCII bytes a multi-byte decoder copies one at a time before a run
/// reads words: in CJK text an ASCII byte between characters is mostly a
/// lone space.
pub(crate) const LONE: usize = 1;

/// Appends to `out`: `ptr` and `cap` are its buffer and capacity, `len`
/// its length as far as written. A `FIXED` sink reserved room for its
/// whole output up front and checks none on a push.
pub(crate) struct Sink<'a, const FIXED: bool = false> {
    out: &'a mut Vec<u8>,
    ptr: *mut u8,
    cap: usize,
    len: usize,
}

impl<const FIXED: bool> Drop for Sink<'_, FIXED> {
    fn drop(&mut self) {
        // SAFETY: every byte below `len` was written by a push, each of
        // which made room for itself first
        unsafe { self.out.set_len(self.len) }
    }
}

impl<'a> Sink<'a> {
    /// A sink appending to `out`, with room for `expect` bytes reserved
    /// up front.
    pub(crate) fn new(out: &'a mut Vec<u8>, expect: usize) -> Self {
        Self::reserved(out, expect)
    }
}

impl<'a> Sink<'a, true> {
    /// A sink appending to `out` with room for `most` bytes, which no
    /// sequence of pushes may exceed, counting the bytes a push writes
    /// past what it counts.
    pub(crate) fn fixed(out: &'a mut Vec<u8>, most: usize) -> Self {
        Self::reserved(out, most)
    }
}

impl<'a, const FIXED: bool> Sink<'a, FIXED> {
    fn reserved(out: &'a mut Vec<u8>, n: usize) -> Self {
        out.reserve(n);
        Sink {
            ptr: out.as_mut_ptr(),
            cap: out.capacity(),
            len: out.len(),
            out,
        }
    }

    /// Room for `n` more bytes.
    #[inline(always)]
    fn room(&mut self, n: usize) {
        if FIXED {
            // the constructor's caller bounded every push sequence
            debug_assert!(self.cap - self.len >= n, "a fixed sink overflowed");
        } else if self.cap - self.len < n {
            (self.ptr, self.cap) = grow(self.out, self.len, n);
        }
    }

    /// Run `f` on the vector itself, for a decoder's rarely taken steps.
    #[inline(always)]
    pub(crate) fn with_vec<R>(&mut self, f: impl FnOnce(&mut Vec<u8>) -> R) -> R {
        // SAFETY: as in drop
        unsafe { self.out.set_len(self.len) };
        let r = f(self.out);
        self.ptr = self.out.as_mut_ptr();
        self.cap = self.out.capacity();
        self.len = self.out.len();
        r
    }

    /// Write `bytes` at the end and count the first `n` of them; the
    /// rest stay past the length, overwritten by the next push.
    #[inline(always)]
    fn put<const N: usize>(&mut self, bytes: [u8; N], n: usize) {
        debug_assert!(n <= N);
        self.room(N);
        // SAFETY: `room` left at least N bytes of capacity past `len`, or
        // for a fixed sink its caller's bound did
        unsafe {
            let at = self.ptr.add(self.len);
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), at, N);
        }
        self.len += n;
    }

    /// Append the byte `b`, an ASCII byte.
    #[inline(always)]
    pub(crate) fn ascii(&mut self, b: u8) {
        self.put([b], 1);
    }

    /// Append the code point `cp` in UTF-8; `cp` is a scalar value.
    #[inline(always)]
    pub(crate) fn scalar(&mut self, cp: u32) {
        debug_assert!(char::from_u32(cp).is_some());
        if (0x800..0x1_0000).contains(&cp) {
            let b = [
                0xE0 | (cp >> 12) as u8,
                0x80 | ((cp >> 6) & 0x3F) as u8,
                0x80 | (cp & 0x3F) as u8,
                0,
            ];
            self.put(b, 3);
        } else if cp < 0x80 {
            self.put([cp as u8], 1);
        } else if cp < 0x800 {
            self.put([0xC0 | (cp >> 6) as u8, 0x80 | (cp & 0x3F) as u8], 2);
        } else {
            let b = [
                0xF0 | (cp >> 18) as u8,
                0x80 | ((cp >> 12) & 0x3F) as u8,
                0x80 | ((cp >> 6) & 0x3F) as u8,
                0x80 | (cp & 0x3F) as u8,
            ];
            self.put(b, 4);
        }
    }

    /// Append `cp`, U+0800 to U+FFFF and no surrogate, in UTF-8.
    #[inline(always)]
    pub(crate) fn three(&mut self, cp: u32) {
        debug_assert!((0x800..0x1_0000).contains(&cp) && char::from_u32(cp).is_some());
        let b = [
            0xE0 | (cp >> 12) as u8,
            0x80 | ((cp >> 6) & 0x3F) as u8,
            0x80 | (cp & 0x3F) as u8,
            0,
        ];
        self.put(b, 3);
    }

    /// Append `cp`, U+0080 to U+FFFF, in UTF-8.
    #[inline(always)]
    pub(crate) fn past_ascii(&mut self, cp: u32) {
        debug_assert!((0x80..0x1_0000).contains(&cp));
        if cp < 0x800 {
            self.put([0xC0 | (cp >> 6) as u8, 0x80 | (cp & 0x3F) as u8], 2);
        } else {
            let b = [
                0xE0 | (cp >> 12) as u8,
                0x80 | ((cp >> 6) & 0x3F) as u8,
                0x80 | (cp & 0x3F) as u8,
                0,
            ];
            self.put(b, 3);
        }
    }

    /// Append `c` in UTF-8.
    #[inline(always)]
    pub(crate) fn char(&mut self, c: char) {
        self.scalar(u32::from(c));
    }

    /// A decoder's error step: an error when `fatal`, else one U+FFFD.
    #[inline(always)]
    pub(crate) fn error(&mut self, fatal: bool) -> Result<(), DecodeError> {
        if fatal {
            return Err(DecodeError);
        }
        self.put(REPLACEMENT, 3);
        Ok(())
    }

    /// Copy the ASCII byte `b` at `i` of `input` and the run of ASCII
    /// bytes after it, and return where the run ends. The first `SHORT`
    /// bytes go one at a time: a short run, as between the words of a
    /// script past ASCII, ends sooner than a word read finds its end.
    #[inline(always)]
    pub(crate) fn ascii_from<const SHORT: usize>(
        &mut self,
        input: &[u8],
        mut i: usize,
        b: u8,
    ) -> usize {
        self.ascii(b);
        i += 1;
        for _ in 1..SHORT {
            match input.get(i) {
                Some(&next) if next.is_ascii() => self.ascii(next),
                _ => return i,
            }
            i += 1;
        }
        match input.get(i) {
            Some(next) if next.is_ascii() => self.ascii_run(input, i),
            _ => i,
        }
    }

    /// Copy the run of ASCII bytes of `input` from `i` on and return
    /// where it ends.
    #[inline(always)]
    pub(crate) fn ascii_run(&mut self, input: &[u8], i: usize) -> usize {
        self.run(input, i, |w| w & HIGH)
    }

    /// Copy the run of bytes of `input` from `i` on that decode as
    /// themselves and return where it ends. `stop` marks, with the high
    /// bit of each byte, the bytes of a little-endian word that end the
    /// run; it marks every byte with its own high bit set, and its
    /// lowest mark is exact.
    #[inline(always)]
    pub(crate) fn run(&mut self, input: &[u8], mut i: usize, stop: impl Fn(u64) -> u64) -> usize {
        // while whole: sixteen bytes a step, the next step's start
        // independent of where a run would end
        while let Some(chunk) = input.get(i..i + 16) {
            let mut bytes = [0; 16];
            bytes.copy_from_slice(chunk);
            let lo = u64::from_le_bytes(chunk[..8].try_into().expect("eight bytes"));
            let hi = u64::from_le_bytes(chunk[8..].try_into().expect("eight bytes"));
            if stop(lo) | stop(hi) != 0 {
                break;
            }
            self.put(bytes, 16);
            i += 16;
        }
        while let Some(chunk) = input.get(i..i + 8) {
            let mut bytes = [0; 8];
            bytes.copy_from_slice(chunk);
            let marks = stop(u64::from_le_bytes(bytes));
            if marks == 0 {
                self.put(bytes, 8);
                i += 8;
                continue;
            }
            let n = (marks.trailing_zeros() / 8) as usize;
            self.put(bytes, n);
            return i + n;
        }
        while let Some(&b) = input.get(i) {
            if stop(u64::from(b)) & 0x80 != 0 {
                break;
            }
            self.ascii(b);
            i += 1;
        }
        i
    }
}

/// Room for `n` more bytes past `len` in `out`, whose bytes below `len`
/// are written; its buffer and capacity after. A function of its own so
/// that a sink stays in registers.
#[cold]
#[inline(never)]
fn grow(out: &mut Vec<u8>, len: usize, n: usize) -> (*mut u8, usize) {
    // SAFETY: the caller wrote every byte below `len`
    unsafe { out.set_len(len) };
    out.reserve(n);
    (out.as_mut_ptr(), out.capacity())
}

/// The high bit of each byte of `w` that equals `b`, exact up to the
/// lowest such byte (a borrow can mark bytes above it).
#[inline(always)]
pub(crate) fn bytes_equal(w: u64, b: u8) -> u64 {
    let x = w ^ (0x0101_0101_0101_0101 * u64::from(b));
    x.wrapping_sub(0x0101_0101_0101_0101) & !x & HIGH
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_count_only_what_they_claim() {
        let mut out = Vec::from(&b"ab"[..]);
        {
            let mut s = Sink::new(&mut out, 0);
            for cp in [0x41, 0xE9, 0x20AC, 0x1F600, 0x10FFFF] {
                s.scalar(cp);
            }
            assert_eq!(s.error(true), Err(DecodeError));
            s.error(false).expect("not fatal");
        }
        let want = "abA\u{E9}\u{20AC}\u{1F600}\u{10FFFF}\u{FFFD}";
        assert_eq!(out, want.as_bytes());
    }

    #[test]
    fn runs_stop_at_the_first_marked_byte() {
        let input = b"0123456789abcdef\x80ghijklmnopqrstuvwxyz\x1Bq";
        for start in 0..input.len() {
            let mut out = Vec::new();
            let end = Sink::new(&mut out, 0).ascii_run(input, start);
            let want = (start..input.len()).find(|&i| input[i] >= 0x80);
            assert_eq!(end, want.unwrap_or(input.len()), "from {start}");
            assert_eq!(out, &input[start..end]);
            let mut out = Vec::new();
            let stop = |w| (w & HIGH) | bytes_equal(w, 0x1B);
            let end = Sink::new(&mut out, 0).run(input, start, stop);
            let want = (start..input.len()).find(|&i| input[i] >= 0x80 || input[i] == 0x1B);
            assert_eq!(end, want.unwrap_or(input.len()), "from {start}");
            assert_eq!(out, &input[start..end]);
        }
    }
}
