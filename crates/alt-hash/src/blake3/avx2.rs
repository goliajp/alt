//! eight lanes in 256-bit vectors

use super::lanes::{Batch, Lanes, in_groups};
use super::sse41;
use std::arch::x86_64::*;

// The `unsafe` blocks below only wrap vector instructions. Those on registers are
// sound on any CPU with the features of the function they are inlined into; the
// loads and stores access exactly the bytes their callers are documented to own.

/// Rows of an 8x8 matrix of words to its columns.
#[inline(always)]
unsafe fn transpose(rows: [__m256i; 8]) -> [__m256i; 8] {
    unsafe {
        // the unpack instructions work inside each 128-bit half, so after two rounds
        // of them every vector holds two columns' halves: columns n and n + 4
        let ab_0145 = _mm256_unpacklo_epi32(rows[0], rows[1]);
        let ab_2367 = _mm256_unpackhi_epi32(rows[0], rows[1]);
        let cd_0145 = _mm256_unpacklo_epi32(rows[2], rows[3]);
        let cd_2367 = _mm256_unpackhi_epi32(rows[2], rows[3]);
        let ef_0145 = _mm256_unpacklo_epi32(rows[4], rows[5]);
        let ef_2367 = _mm256_unpackhi_epi32(rows[4], rows[5]);
        let gh_0145 = _mm256_unpacklo_epi32(rows[6], rows[7]);
        let gh_2367 = _mm256_unpackhi_epi32(rows[6], rows[7]);
        let abcd_04 = _mm256_unpacklo_epi64(ab_0145, cd_0145);
        let abcd_15 = _mm256_unpackhi_epi64(ab_0145, cd_0145);
        let abcd_26 = _mm256_unpacklo_epi64(ab_2367, cd_2367);
        let abcd_37 = _mm256_unpackhi_epi64(ab_2367, cd_2367);
        let efgh_04 = _mm256_unpacklo_epi64(ef_0145, gh_0145);
        let efgh_15 = _mm256_unpackhi_epi64(ef_0145, gh_0145);
        let efgh_26 = _mm256_unpacklo_epi64(ef_2367, gh_2367);
        let efgh_37 = _mm256_unpackhi_epi64(ef_2367, gh_2367);
        [
            _mm256_permute2x128_si256::<0x20>(abcd_04, efgh_04),
            _mm256_permute2x128_si256::<0x20>(abcd_15, efgh_15),
            _mm256_permute2x128_si256::<0x20>(abcd_26, efgh_26),
            _mm256_permute2x128_si256::<0x20>(abcd_37, efgh_37),
            _mm256_permute2x128_si256::<0x31>(abcd_04, efgh_04),
            _mm256_permute2x128_si256::<0x31>(abcd_15, efgh_15),
            _mm256_permute2x128_si256::<0x31>(abcd_26, efgh_26),
            _mm256_permute2x128_si256::<0x31>(abcd_37, efgh_37),
        ]
    }
}

#[inline(always)]
unsafe fn load(at: *const u8) -> __m256i {
    unsafe { _mm256_loadu_si256(at.cast()) }
}

/// eight words from each of eight lanes, as one vector per word
#[inline(always)]
unsafe fn half(blocks: *const u8, stride: usize) -> [__m256i; 8] {
    unsafe {
        transpose([
            load(blocks),
            load(blocks.add(stride)),
            load(blocks.add(2 * stride)),
            load(blocks.add(3 * stride)),
            load(blocks.add(4 * stride)),
            load(blocks.add(5 * stride)),
            load(blocks.add(6 * stride)),
            load(blocks.add(7 * stride)),
        ])
    }
}

impl Lanes for __m256i {
    const N: usize = 8;

    #[inline(always)]
    unsafe fn splat(word: u32) -> Self {
        unsafe { _mm256_set1_epi32(word as i32) }
    }

    #[inline(always)]
    unsafe fn words(words: &[u32; 8]) -> Self {
        unsafe { load(words.as_ptr().cast()) }
    }

    #[inline(always)]
    unsafe fn add(self, other: Self) -> Self {
        unsafe { _mm256_add_epi32(self, other) }
    }

    #[inline(always)]
    unsafe fn xor(self, other: Self) -> Self {
        unsafe { _mm256_xor_si256(self, other) }
    }

    #[inline(always)]
    unsafe fn rot16(self) -> Self {
        unsafe {
            let bytes = _mm256_setr_epi8(
                2, 3, 0, 1, 6, 7, 4, 5, 10, 11, 8, 9, 14, 15, 12, 13, 2, 3, 0, 1, 6, 7, 4, 5, 10,
                11, 8, 9, 14, 15, 12, 13,
            );
            _mm256_shuffle_epi8(self, bytes)
        }
    }

    #[inline(always)]
    unsafe fn rot12(self) -> Self {
        unsafe { _mm256_or_si256(_mm256_srli_epi32::<12>(self), _mm256_slli_epi32::<20>(self)) }
    }

    #[inline(always)]
    unsafe fn rot8(self) -> Self {
        unsafe {
            let bytes = _mm256_setr_epi8(
                1, 2, 3, 0, 5, 6, 7, 4, 9, 10, 11, 8, 13, 14, 15, 12, 1, 2, 3, 0, 5, 6, 7, 4, 9,
                10, 11, 8, 13, 14, 15, 12,
            );
            _mm256_shuffle_epi8(self, bytes)
        }
    }

    #[inline(always)]
    unsafe fn rot7(self) -> Self {
        unsafe { _mm256_or_si256(_mm256_srli_epi32::<7>(self), _mm256_slli_epi32::<25>(self)) }
    }

    #[inline(always)]
    unsafe fn load(blocks: *const u8, stride: usize) -> [Self; 16] {
        unsafe {
            let [m0, m1, m2, m3, m4, m5, m6, m7] = half(blocks, stride);
            let [m8, m9, m10, m11, m12, m13, m14, m15] = half(blocks.add(32), stride);
            [
                m0, m1, m2, m3, m4, m5, m6, m7, m8, m9, m10, m11, m12, m13, m14, m15,
            ]
        }
    }

    #[inline(always)]
    unsafe fn store(h: &[Self; 8], out: *mut u8) {
        unsafe {
            for (lane, cv) in transpose(*h).into_iter().enumerate() {
                _mm256_storeu_si256(out.add(lane * 32).cast(), cv);
            }
        }
    }
}

/// Compresses the nodes in `input` eight at a time, then four at a time, and
/// returns what is left of the input and of `out`, and the counter of the next node.
#[target_feature(enable = "avx2")]
pub(super) unsafe fn hash_many<'a, 'b>(
    batch: &Batch,
    input: &'a [u8],
    counter: u64,
    out: &'b mut [u8],
) -> (&'a [u8], u64, &'b mut [u8]) {
    // avx2 is enabled for this function, and it implies sse4.1
    unsafe {
        let (rest, counter, out) = in_groups::<__m256i>(batch, input, counter, out);
        sse41::hash_many(batch, rest, counter, out)
    }
}
