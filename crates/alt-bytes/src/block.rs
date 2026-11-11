//! Where a byte occurs in a block of sixteen: one vector comparison on the
//! instruction sets every x86_64 and aarch64 processor has, eight bytes at a time
//! in a register elsewhere.

pub(crate) const BLOCK: usize = 16;

/// A bit set for each byte of `block` that equals `byte`, `STEP` bits apart, the
/// first byte lowest.
#[cfg(target_arch = "x86_64")]
#[inline(always)]
fn hits(block: &[u8; BLOCK], byte: u8) -> u64 {
    use std::arch::x86_64::*;
    // SSE2 is part of x86_64, and the load reads the sixteen bytes of `block`
    unsafe {
        let equal = _mm_cmpeq_epi8(
            _mm_loadu_si128(block.as_ptr().cast()),
            _mm_set1_epi8(byte as i8),
        );
        _mm_movemask_epi8(equal) as u32 as u64
    }
}
#[cfg(target_arch = "x86_64")]
const STEP: u32 = 1;

#[cfg(target_arch = "aarch64")]
#[inline(always)]
fn hits(block: &[u8; BLOCK], byte: u8) -> u64 {
    use std::arch::aarch64::*;
    // NEON is part of aarch64, and the load reads the sixteen bytes of `block`
    unsafe {
        let equal = vceqq_u8(vld1q_u8(block.as_ptr()), vdupq_n_u8(byte));
        // there is no instruction that takes one bit per byte; narrowing each
        // pair of bytes by four bits leaves four bits per byte in a register
        let nibbles = vshrn_n_u16::<4>(vreinterpretq_u16_u8(equal));
        vget_lane_u64::<0>(vreinterpret_u64_u8(nibbles)) & 0x1111_1111_1111_1111
    }
}
#[cfg(target_arch = "aarch64")]
const STEP: u32 = 4;

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
use portable as hits;

/// `hits` with one bit per byte, in plain arithmetic
#[cfg(any(test, not(any(target_arch = "x86_64", target_arch = "aarch64"))))]
#[inline(always)]
fn portable(block: &[u8; BLOCK], byte: u8) -> u64 {
    const LOW: u64 = 0x0101_0101_0101_0101;
    const HIGH: u64 = 0x8080_8080_8080_8080;
    let (halves, _) = block.as_chunks::<8>();
    let mut bits = 0;
    for (half, word) in halves.iter().enumerate() {
        let diff = u64::from_le_bytes(*word) ^ (LOW * u64::from(byte));
        // adding 0x7f to the low seven bits carries into the high bit exactly
        // when one of them is set; no carry crosses a byte boundary, so every
        // reported position is exact
        let equal = !(((diff & !HIGH) + !HIGH) | diff) & HIGH;
        // one bit per byte: the high bits gathered into the top byte
        let packed = (equal >> 7).wrapping_mul(0x0102_0408_1020_4080) >> 56;
        bits |= packed << (8 * half);
    }
    bits
}
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
const STEP: u32 = 1;

/// the index of the first byte of `block` that equals `byte`
#[inline(always)]
pub(crate) fn first(block: &[u8; BLOCK], byte: u8) -> Option<usize> {
    let hits = hits(block, byte);
    (hits != 0).then(|| (hits.trailing_zeros() / STEP) as usize)
}

/// the index of the last byte of `block` that equals `byte`
#[inline(always)]
pub(crate) fn last(block: &[u8; BLOCK], byte: u8) -> Option<usize> {
    let hits = hits(block, byte);
    (hits != 0).then(|| ((63 - hits.leading_zeros()) / STEP) as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_portable_comparison_finds_the_same_bytes() {
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        for _ in 0..20_000 {
            let mut block = [0u8; BLOCK];
            for byte in &mut block {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                // few distinct values, so that blocks hold several matches or none
                *byte = (state >> 60) as u8;
            }
            let byte = (state >> 40) as u8 & 0x0f;
            let expected: u64 = (0..BLOCK)
                .filter(|&at| block[at] == byte)
                .map(|at| 1 << at)
                .sum();
            assert_eq!(portable(&block, byte), expected);
            let first = (expected != 0).then(|| expected.trailing_zeros() as usize);
            let last = (expected != 0).then(|| 63 - expected.leading_zeros() as usize);
            assert_eq!(super::first(&block, byte), first);
            assert_eq!(super::last(&block, byte), last);
        }
    }
}
