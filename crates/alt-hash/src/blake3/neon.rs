//! four lanes in 128-bit NEON vectors

use super::MAX_LANES;
use super::lanes::{Batch, Lanes, Short, compress_ragged, in_groups};
use std::arch::aarch64::*;

// The `unsafe` blocks below only wrap vector instructions. Those on registers are
// sound on any CPU with the features of the function they are inlined into; the
// loads and stores access exactly the bytes their callers are documented to own.

/// Rows `a b c d` of a 4x4 matrix of words to its columns.
#[inline(always)]
unsafe fn transpose(a: uint32x4_t, b: uint32x4_t, c: uint32x4_t, d: uint32x4_t) -> [uint32x4_t; 4] {
    unsafe {
        let ab_02 = vreinterpretq_u64_u32(vtrn1q_u32(a, b));
        let ab_13 = vreinterpretq_u64_u32(vtrn2q_u32(a, b));
        let cd_02 = vreinterpretq_u64_u32(vtrn1q_u32(c, d));
        let cd_13 = vreinterpretq_u64_u32(vtrn2q_u32(c, d));
        [
            vreinterpretq_u32_u64(vtrn1q_u64(ab_02, cd_02)),
            vreinterpretq_u32_u64(vtrn1q_u64(ab_13, cd_13)),
            vreinterpretq_u32_u64(vtrn2q_u64(ab_02, cd_02)),
            vreinterpretq_u32_u64(vtrn2q_u64(ab_13, cd_13)),
        ]
    }
}

/// sixteen bytes at `at` as four little-endian words
#[inline(always)]
unsafe fn load(at: *const u8) -> uint32x4_t {
    unsafe { vreinterpretq_u32_u8(vld1q_u8(at)) }
}

/// four words from each of four lanes, as one vector per word
#[inline(always)]
unsafe fn quarter(blocks: *const u8, stride: usize) -> [uint32x4_t; 4] {
    unsafe {
        transpose(
            load(blocks),
            load(blocks.add(stride)),
            load(blocks.add(2 * stride)),
            load(blocks.add(3 * stride)),
        )
    }
}

impl Lanes for uint32x4_t {
    const N: usize = 4;

    #[inline(always)]
    unsafe fn splat(word: u32) -> Self {
        unsafe { vdupq_n_u32(word) }
    }

    #[inline(always)]
    unsafe fn words(words: &[u32; 8]) -> Self {
        unsafe { vld1q_u32(words.as_ptr()) }
    }

    #[inline(always)]
    unsafe fn add(self, other: Self) -> Self {
        unsafe { vaddq_u32(self, other) }
    }

    #[inline(always)]
    unsafe fn xor(self, other: Self) -> Self {
        unsafe { veorq_u32(self, other) }
    }

    #[inline(always)]
    unsafe fn rot16(self) -> Self {
        unsafe { vreinterpretq_u32_u16(vrev32q_u16(vreinterpretq_u16_u32(self))) }
    }

    #[inline(always)]
    unsafe fn rot12(self) -> Self {
        unsafe { vsliq_n_u32::<20>(vshrq_n_u32::<12>(self), self) }
    }

    #[inline(always)]
    unsafe fn rot8(self) -> Self {
        const BYTES: [u8; 16] = [1, 2, 3, 0, 5, 6, 7, 4, 9, 10, 11, 8, 13, 14, 15, 12];
        unsafe {
            vreinterpretq_u32_u8(vqtbl1q_u8(
                vreinterpretq_u8_u32(self),
                vld1q_u8(BYTES.as_ptr()),
            ))
        }
    }

    #[inline(always)]
    unsafe fn rot7(self) -> Self {
        unsafe { vsliq_n_u32::<25>(vshrq_n_u32::<7>(self), self) }
    }

    #[inline(always)]
    unsafe fn load(blocks: *const u8, stride: usize) -> [Self; 16] {
        unsafe {
            let [m0, m1, m2, m3] = quarter(blocks, stride);
            let [m4, m5, m6, m7] = quarter(blocks.add(16), stride);
            let [m8, m9, m10, m11] = quarter(blocks.add(32), stride);
            let [m12, m13, m14, m15] = quarter(blocks.add(48), stride);
            [
                m0, m1, m2, m3, m4, m5, m6, m7, m8, m9, m10, m11, m12, m13, m14, m15,
            ]
        }
    }

    #[inline(always)]
    unsafe fn gather(blocks: &[*const u8; MAX_LANES]) -> [Self; 16] {
        unsafe {
            let quarter = |at: usize| {
                transpose(
                    load(blocks[0].add(at)),
                    load(blocks[1].add(at)),
                    load(blocks[2].add(at)),
                    load(blocks[3].add(at)),
                )
            };
            let [m0, m1, m2, m3] = quarter(0);
            let [m4, m5, m6, m7] = quarter(16);
            let [m8, m9, m10, m11] = quarter(32);
            let [m12, m13, m14, m15] = quarter(48);
            [
                m0, m1, m2, m3, m4, m5, m6, m7, m8, m9, m10, m11, m12, m13, m14, m15,
            ]
        }
    }

    #[inline(always)]
    unsafe fn store(h: &[Self; 8], out: *mut u8) {
        unsafe {
            let low = transpose(h[0], h[1], h[2], h[3]);
            let high = transpose(h[4], h[5], h[6], h[7]);
            for lane in 0..4 {
                vst1q_u8(out.add(lane * 32), vreinterpretq_u8_u32(low[lane]));
                vst1q_u8(out.add(lane * 32 + 16), vreinterpretq_u8_u32(high[lane]));
            }
        }
    }
}

/// Two groups of four lanes whose instructions alternate. One group leaves vector
/// units idle on a core with four of them, because each of its four additions per
/// step waits for the one before.
#[derive(Clone, Copy)]
pub(super) struct Pair(uint32x4_t, uint32x4_t);

macro_rules! both {
    ($name:ident) => {
        #[inline(always)]
        unsafe fn $name(self) -> Self {
            unsafe { Self(self.0.$name(), self.1.$name()) }
        }
    };
    ($name:ident, $other:ident) => {
        #[inline(always)]
        unsafe fn $name(self, $other: Self) -> Self {
            unsafe { Self(self.0.$name($other.0), self.1.$name($other.1)) }
        }
    };
}

impl Lanes for Pair {
    const N: usize = 8;

    #[inline(always)]
    unsafe fn splat(word: u32) -> Self {
        unsafe { Self(vdupq_n_u32(word), vdupq_n_u32(word)) }
    }

    #[inline(always)]
    unsafe fn words(words: &[u32; 8]) -> Self {
        unsafe { Self(vld1q_u32(words.as_ptr()), vld1q_u32(words.as_ptr().add(4))) }
    }

    both!(add, other);
    both!(xor, other);
    both!(rot16);
    both!(rot12);
    both!(rot8);
    both!(rot7);

    #[inline(always)]
    unsafe fn load(blocks: *const u8, stride: usize) -> [Self; 16] {
        unsafe {
            let first = uint32x4_t::load(blocks, stride);
            let second = uint32x4_t::load(blocks.add(4 * stride), stride);
            std::array::from_fn(|word| Self(first[word], second[word]))
        }
    }

    #[inline(always)]
    unsafe fn store(h: &[Self; 8], out: *mut u8) {
        unsafe {
            uint32x4_t::store(&h.map(|pair| pair.0), out);
            uint32x4_t::store(&h.map(|pair| pair.1), out.add(4 * 32));
        }
    }
}

/// Compresses the nodes in `input` four at a time and returns what is left of the
/// input and of `out`, and the counter of the next node.
#[target_feature(enable = "neon")]
pub(super) unsafe fn hash_many<'a, 'b>(
    batch: &Batch,
    input: &'a [u8],
    counter: u64,
    out: &'b mut [u8],
) -> (&'a [u8], u64, &'b mut [u8]) {
    // neon is enabled for this function
    unsafe { in_groups::<uint32x4_t>(batch, input, counter, out) }
}

/// `hash_many` taking eight nodes at a time while there are that many
#[target_feature(enable = "neon")]
pub(super) unsafe fn hash_many_paired<'a, 'b>(
    batch: &Batch,
    input: &'a [u8],
    counter: u64,
    out: &'b mut [u8],
) -> (&'a [u8], u64, &'b mut [u8]) {
    // neon is enabled for this function
    unsafe {
        let (rest, counter, out) = in_groups::<Pair>(batch, input, counter, out);
        in_groups::<uint32x4_t>(batch, rest, counter, out)
    }
}

/// Compresses at most four chunks of any lengths side by side; `out` gets 32 bytes
/// for each.
#[target_feature(enable = "neon")]
pub(super) unsafe fn ragged(key: &[u32; 8], flags: u32, chunks: &[Short], out: &mut [u8]) {
    // neon is enabled for this function
    unsafe { compress_ragged::<uint32x4_t>(key, flags, chunks, out) }
}
