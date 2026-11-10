//! four lanes in 128-bit vectors, and the single-block compression with the state
//! as four rows of four words

use super::IV;
use super::lanes::{Batch, Lanes, in_groups};
use std::arch::x86_64::*;

// The `unsafe` blocks below only wrap vector instructions. Those on registers are
// sound on any CPU with the features of the function they are inlined into; the
// loads and stores access exactly the bytes their callers are documented to own.

/// the immediate that selects words `a b c d`, in that order, in a shuffle
const fn pick(a: i32, b: i32, c: i32, d: i32) -> i32 {
    a | b << 2 | c << 4 | d << 6
}

/// words `a b` of `x` followed by words `c d` of `y`
#[inline(always)]
unsafe fn pair<const WORDS: i32>(x: __m128i, y: __m128i) -> __m128i {
    unsafe {
        _mm_castps_si128(_mm_shuffle_ps::<WORDS>(
            _mm_castsi128_ps(x),
            _mm_castsi128_ps(y),
        ))
    }
}

/// rows `a b c d` of a 4x4 matrix of words to its columns
#[inline(always)]
unsafe fn transpose(a: __m128i, b: __m128i, c: __m128i, d: __m128i) -> [__m128i; 4] {
    unsafe {
        let ab_01 = _mm_unpacklo_epi32(a, b);
        let ab_23 = _mm_unpackhi_epi32(a, b);
        let cd_01 = _mm_unpacklo_epi32(c, d);
        let cd_23 = _mm_unpackhi_epi32(c, d);
        [
            _mm_unpacklo_epi64(ab_01, cd_01),
            _mm_unpackhi_epi64(ab_01, cd_01),
            _mm_unpacklo_epi64(ab_23, cd_23),
            _mm_unpackhi_epi64(ab_23, cd_23),
        ]
    }
}

#[inline(always)]
unsafe fn load(at: *const u8) -> __m128i {
    unsafe { _mm_loadu_si128(at.cast()) }
}

/// four words from each of four lanes, as one vector per word
#[inline(always)]
unsafe fn quarter(blocks: *const u8, stride: usize) -> [__m128i; 4] {
    unsafe {
        transpose(
            load(blocks),
            load(blocks.add(stride)),
            load(blocks.add(2 * stride)),
            load(blocks.add(3 * stride)),
        )
    }
}

impl Lanes for __m128i {
    const N: usize = 4;

    #[inline(always)]
    unsafe fn splat(word: u32) -> Self {
        unsafe { _mm_set1_epi32(word as i32) }
    }

    #[inline(always)]
    unsafe fn words(words: &[u32; 8]) -> Self {
        unsafe { load(words.as_ptr().cast()) }
    }

    #[inline(always)]
    unsafe fn add(self, other: Self) -> Self {
        unsafe { _mm_add_epi32(self, other) }
    }

    #[inline(always)]
    unsafe fn xor(self, other: Self) -> Self {
        unsafe { _mm_xor_si128(self, other) }
    }

    #[inline(always)]
    unsafe fn rot16(self) -> Self {
        unsafe {
            let bytes = _mm_setr_epi8(2, 3, 0, 1, 6, 7, 4, 5, 10, 11, 8, 9, 14, 15, 12, 13);
            _mm_shuffle_epi8(self, bytes)
        }
    }

    #[inline(always)]
    unsafe fn rot12(self) -> Self {
        unsafe { _mm_or_si128(_mm_srli_epi32::<12>(self), _mm_slli_epi32::<20>(self)) }
    }

    #[inline(always)]
    unsafe fn rot8(self) -> Self {
        unsafe {
            let bytes = _mm_setr_epi8(1, 2, 3, 0, 5, 6, 7, 4, 9, 10, 11, 8, 13, 14, 15, 12);
            _mm_shuffle_epi8(self, bytes)
        }
    }

    #[inline(always)]
    unsafe fn rot7(self) -> Self {
        unsafe { _mm_or_si128(_mm_srli_epi32::<7>(self), _mm_slli_epi32::<25>(self)) }
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
    unsafe fn store(h: &[Self; 8], out: *mut u8) {
        unsafe {
            let low = transpose(h[0], h[1], h[2], h[3]);
            let high = transpose(h[4], h[5], h[6], h[7]);
            for lane in 0..4 {
                _mm_storeu_si128(out.add(lane * 32).cast(), low[lane]);
                _mm_storeu_si128(out.add(lane * 32 + 16).cast(), high[lane]);
            }
        }
    }
}

/// Half of the mixing of all four columns at once: `rows` are the state words
/// 0-3, 4-7, 8-11 and 12-15, `m` one message word per column.
#[inline(always)]
unsafe fn half<const FIRST: bool>(rows: &mut [__m128i; 4], m: __m128i) {
    unsafe {
        rows[0] = rows[0].add(rows[1]).add(m);
        rows[3] = rows[3].xor(rows[0]);
        rows[3] = if FIRST {
            rows[3].rot16()
        } else {
            rows[3].rot8()
        };
        rows[2] = rows[2].add(rows[3]);
        rows[1] = rows[1].xor(rows[2]);
        rows[1] = if FIRST {
            rows[1].rot12()
        } else {
            rows[1].rot7()
        };
    }
}

/// The chaining value that follows `cv` after one block. Inlined into a function for
/// each instruction set: with AVX the same code needs no register copies.
#[inline(always)]
unsafe fn compress_body(
    cv: &[u32; 8],
    block: &[u8; 64],
    block_len: u32,
    counter: u64,
    flags: u32,
) -> [u32; 8] {
    // unaligned loads and stores on references that are as long as what is accessed
    unsafe {
        let value = [load(cv.as_ptr().cast()), load(cv.as_ptr().add(4).cast())];
        let value = compress_rows(value, block, block_len, counter, flags);
        let mut out = [0u32; 8];
        _mm_storeu_si128(out.as_mut_ptr().cast(), value[0]);
        _mm_storeu_si128(out.as_mut_ptr().add(4).cast(), value[1]);
        out
    }
}

/// `compress_body` on a chaining value that is already in two registers
#[inline(always)]
unsafe fn compress_rows(
    value: [__m128i; 2],
    block: &[u8; 64],
    block_len: u32,
    counter: u64,
    flags: u32,
) -> [__m128i; 2] {
    // unaligned loads from references that are as long as what is read
    unsafe {
        let mut rows = [
            value[0],
            value[1],
            load(IV.as_ptr().cast()),
            _mm_set_epi32(
                flags as i32,
                block_len as i32,
                (counter >> 32) as i32,
                counter as i32,
            ),
        ];
        // the message words in the order this round uses them
        let mut a = load(block.as_ptr());
        let mut b = load(block.as_ptr().add(16));
        let mut c = load(block.as_ptr().add(32));
        let mut d = load(block.as_ptr().add(48));
        for _ in 0..7 {
            half::<true>(&mut rows, pair::<{ pick(0, 2, 0, 2) }>(a, b));
            half::<false>(&mut rows, pair::<{ pick(1, 3, 1, 3) }>(a, b));
            // rotating rows 1-3 turns the diagonals into columns
            rows[1] = _mm_shuffle_epi32::<{ pick(1, 2, 3, 0) }>(rows[1]);
            rows[2] = _mm_shuffle_epi32::<{ pick(2, 3, 0, 1) }>(rows[2]);
            rows[3] = _mm_shuffle_epi32::<{ pick(3, 0, 1, 2) }>(rows[3]);
            half::<true>(&mut rows, pair::<{ pick(0, 2, 0, 2) }>(c, d));
            half::<false>(&mut rows, pair::<{ pick(1, 3, 1, 3) }>(c, d));
            rows[1] = _mm_shuffle_epi32::<{ pick(3, 0, 1, 2) }>(rows[1]);
            rows[2] = _mm_shuffle_epi32::<{ pick(2, 3, 0, 1) }>(rows[2]);
            rows[3] = _mm_shuffle_epi32::<{ pick(1, 2, 3, 0) }>(rows[3]);
            // the permutation: a' = a2 b2 a3 c2, b' = b3 a0 b0 d1,
            // c' = a1 c3 d0 b1, d' = c1 d2 d3 c0
            let next_a = _mm_blend_epi16::<0xc0>(
                _mm_unpackhi_epi32(a, b),
                _mm_shuffle_epi32::<{ pick(0, 0, 0, 2) }>(c),
            );
            let next_b = _mm_blend_epi16::<0xcc>(
                _mm_shuffle_epi32::<{ pick(3, 0, 0, 0) }>(b),
                pair::<{ pick(0, 0, 1, 1) }>(a, d),
            );
            let next_c = _mm_blend_epi16::<0x3c>(
                pair::<{ pick(1, 1, 1, 1) }>(a, b),
                pair::<{ pick(3, 3, 0, 0) }>(c, d),
            );
            let next_d =
                _mm_shuffle_epi32::<{ pick(0, 2, 3, 1) }>(pair::<{ pick(1, 0, 2, 3) }>(c, d));
            (a, b, c, d) = (next_a, next_b, next_c, next_d);
        }
        [rows[0].xor(rows[2]), rows[1].xor(rows[3])]
    }
}

/// advances `cv` over whole blocks of one chunk; the first one also gets `flags_start`
#[inline(always)]
unsafe fn absorb_body(
    cv: &mut [u32; 8],
    blocks: &[[u8; 64]],
    counter: u64,
    flags: u32,
    flags_start: u32,
) {
    let mut block_flags = flags | flags_start;
    // the caller has the features enabled; the value stays in registers between blocks
    unsafe {
        let mut value = [load(cv.as_ptr().cast()), load(cv.as_ptr().add(4).cast())];
        for block in blocks {
            value = compress_rows(value, block, 64, counter, block_flags);
            block_flags = flags;
        }
        _mm_storeu_si128(cv.as_mut_ptr().cast(), value[0]);
        _mm_storeu_si128(cv.as_mut_ptr().add(4).cast(), value[1]);
    }
}

macro_rules! instances {
    ($feature:literal, $compress:ident, $absorb:ident) => {
        /// the chaining value that follows `cv` after one block
        #[target_feature(enable = $feature)]
        pub(super) unsafe fn $compress(
            cv: &[u32; 8],
            block: &[u8; 64],
            block_len: u32,
            counter: u64,
            flags: u32,
        ) -> [u32; 8] {
            // the feature is enabled for this function
            unsafe { compress_body(cv, block, block_len, counter, flags) }
        }

        /// advances `cv` over whole blocks of one chunk; the first also gets `flags_start`
        #[target_feature(enable = $feature)]
        pub(super) unsafe fn $absorb(
            cv: &mut [u32; 8],
            blocks: &[[u8; 64]],
            counter: u64,
            flags: u32,
            flags_start: u32,
        ) {
            // the feature is enabled for this function
            unsafe { absorb_body(cv, blocks, counter, flags, flags_start) }
        }
    };
}

instances!("sse4.1", compress, absorb);
instances!("avx2", compress_vex, absorb_vex);

/// Compresses the nodes in `input` four at a time and returns what is left of the
/// input and of `out`, and the counter of the next node.
#[target_feature(enable = "sse4.1")]
pub(super) unsafe fn hash_many<'a, 'b>(
    batch: &Batch,
    input: &'a [u8],
    counter: u64,
    out: &'b mut [u8],
) -> (&'a [u8], u64, &'b mut [u8]) {
    // sse4.1 is enabled for this function
    unsafe { in_groups::<__m128i>(batch, input, counter, out) }
}
