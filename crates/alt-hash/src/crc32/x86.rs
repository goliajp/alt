//! CRC-32 by carry-less multiplication on x86-64.
//!
//! The message is kept as four 128-bit remainders that each stand for a quarter of
//! the data read so far. One step multiplies every remainder ahead by the distance of
//! four and adds the next 64 bytes; at the end the four are merged and reduced.

use super::{POLYNOMIAL, ahead, portable};
use std::arch::x86_64::*;

const AHEAD_4: [u64; 2] = ahead(4 * 128);
const AHEAD_1: [u64; 2] = ahead(128);
const AHEAD_2: [u64; 2] = ahead(2 * 128);
/// x^64, to bring the upper half of a 96-bit remainder down
const AHEAD_HALF: u64 = (super::power(64) as u64) << 1;
/// the polynomial with its top bit, and the quotient of x^64 by it, both reflected
const REDUCE: [u64; 2] = [((POLYNOMIAL as u64) << 1) | 1, quotient()];

const fn quotient() -> u64 {
    // long division of x^64 by the 33-bit polynomial, then the 33 quotient bits reversed
    let polynomial = 0x1_04c1_1db7u128;
    let mut remainder = 1u128 << 64;
    let mut quotient = 0u64;
    let mut bit = 64;
    while bit >= 32 {
        if remainder >> bit & 1 != 0 {
            quotient |= 1 << (bit - 32);
            remainder ^= polynomial << (bit - 32);
        }
        bit -= 1;
    }
    quotient.reverse_bits() >> 31
}

#[inline(always)]
unsafe fn pair(values: [u64; 2]) -> __m128i {
    unsafe { _mm_set_epi64x(values[1] as i64, values[0] as i64) }
}

/// `remainder` moved ahead by the distance of `by`, plus `next`
#[inline(always)]
unsafe fn step(remainder: __m128i, by: __m128i, next: __m128i) -> __m128i {
    unsafe {
        let low = _mm_clmulepi64_si128::<0x00>(remainder, by);
        let high = _mm_clmulepi64_si128::<0x11>(remainder, by);
        _mm_xor_si128(_mm_xor_si128(low, high), next)
    }
}

#[inline(always)]
pub(super) fn supported() -> bool {
    std::arch::is_x86_feature_detected!("pclmulqdq")
        && std::arch::is_x86_feature_detected!("sse4.1")
}

/// advances the register (without inversion) over `data`
#[target_feature(enable = "pclmulqdq,sse2,sse4.1")]
pub(super) unsafe fn fold(register: u32, data: &[u8]) -> u32 {
    if data.len() < 16 {
        return portable(register, data);
    }
    unsafe {
        let load = |bytes: &[u8]| _mm_loadu_si128(bytes.as_ptr().cast());
        let seed = _mm_cvtsi32_si128(register as i32);
        let (mut blocks, rest) = data.as_chunks::<16>();
        let remainder;
        if blocks.len() >= 4 {
            let by = pair(AHEAD_4);
            let mut lanes = [
                _mm_xor_si128(load(&blocks[0]), seed),
                load(&blocks[1]),
                load(&blocks[2]),
                load(&blocks[3]),
            ];
            blocks = &blocks[4..];
            while let Some((four, tail)) = blocks.split_first_chunk::<4>() {
                for (lane, block) in lanes.iter_mut().zip(four) {
                    *lane = step(*lane, by, load(block));
                }
                blocks = tail;
            }
            // two pairs side by side, then the pair of results
            let by = pair(AHEAD_1);
            let first = step(lanes[0], by, lanes[1]);
            let second = step(lanes[2], by, lanes[3]);
            remainder = step(first, pair(AHEAD_2), second);
        } else {
            remainder = _mm_xor_si128(load(&blocks[0]), seed);
            blocks = &blocks[1..];
        }
        finish(remainder, blocks, rest)
    }
}

/// folds the remaining whole blocks into the remainder, reduces it to the register
/// and finishes the last bytes
#[inline]
#[target_feature(enable = "pclmulqdq,sse2,sse4.1")]
unsafe fn finish(mut remainder: __m128i, blocks: &[[u8; 16]], rest: &[u8]) -> u32 {
    unsafe {
        let load = |bytes: &[u8]| _mm_loadu_si128(bytes.as_ptr().cast());
        let by = pair(AHEAD_1);
        for block in blocks {
            remainder = step(remainder, by, load(block));
        }
        // 128 bits to 96: the low half times x^(128-32) onto the high half
        let low_words = _mm_setr_epi32(!0, 0, !0, 0);
        let folded = _mm_xor_si128(
            _mm_srli_si128::<8>(remainder),
            _mm_clmulepi64_si128::<0x10>(remainder, by),
        );
        // 96 bits to 64: the lowest word times x^64 onto the rest
        let folded = _mm_xor_si128(
            _mm_srli_si128::<4>(folded),
            _mm_clmulepi64_si128::<0x00>(
                _mm_and_si128(folded, low_words),
                _mm_cvtsi64_si128(AHEAD_HALF as i64),
            ),
        );
        // 64 bits to 32 without a division: estimate the quotient by the polynomial,
        // multiply back, and what is left is the remainder
        let reduce = pair(REDUCE);
        let estimate = _mm_and_si128(
            _mm_clmulepi64_si128::<0x10>(_mm_and_si128(folded, low_words), reduce),
            low_words,
        );
        let folded = _mm_xor_si128(folded, _mm_clmulepi64_si128::<0x00>(estimate, reduce));
        portable(_mm_extract_epi32::<1>(folded) as u32, rest)
    }
}
