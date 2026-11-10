//! Adler-32, the checksum of zlib streams: two sums modulo 65521, the bytes and the
//! running totals of the bytes.

/// the most bytes whose sums cannot overflow 32 bits before a reduction
const CHUNK: usize = 5552;
const MODULUS: u32 = 65521;

/// continues an Adler-32 over `data`; a fresh checksum starts from 1
pub fn adler32(checksum: u32, data: &[u8]) -> u32 {
    let (mut low, mut high) = (checksum & 0xffff, checksum >> 16);
    for chunk in data.chunks(CHUNK) {
        let rest = wide(&mut low, &mut high, chunk);
        for byte in rest {
            low += u32::from(*byte);
            high += low;
        }
        low %= MODULUS;
        high %= MODULUS;
    }
    high << 16 | low
}

/// adds the whole 32-byte blocks of `chunk` to the sums and returns what is left
#[inline]
fn wide<'a>(low: &mut u32, high: &mut u32, chunk: &'a [u8]) -> &'a [u8] {
    let (blocks, rest) = chunk.as_chunks::<32>();
    if blocks.is_empty() {
        return rest;
    }
    #[cfg(target_arch = "aarch64")]
    {
        // NEON is part of the baseline on this architecture
        let (sum, weighted) = unsafe { neon(blocks) };
        *high += *low * (blocks.len() as u32 * 32) + weighted;
        *low += sum;
        rest
    }
    #[cfg(target_arch = "x86_64")]
    {
        if std::arch::is_x86_feature_detected!("avx2") {
            // the feature was just detected on this CPU
            let (sum, weighted) = unsafe { avx2(blocks) };
            *high += *low * (blocks.len() as u32 * 32) + weighted;
            *low += sum;
            return rest;
        }
        chunk
    }
    #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
    {
        chunk
    }
}

/// The sum of the bytes, and the sum of every byte times the number of bytes from it
/// to the end, over at most 173 blocks.
#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn neon(blocks: &[[u8; 32]]) -> (u32, u32) {
    use std::arch::aarch64::*;
    unsafe {
        // `before` adds up the byte sum as it stood before each block: every earlier
        // byte counts once more per block that follows it
        let mut before = vdupq_n_u32(0);
        let mut sum = vdupq_n_u32(0);
        // per position in the block; 173 blocks of 255 stay below 16 bits
        let mut columns = [vdupq_n_u16(0); 4];
        for block in blocks {
            let first = vld1q_u8(block.as_ptr());
            let second = vld1q_u8(block.as_ptr().add(16));
            before = vaddq_u32(before, sum);
            sum = vpadalq_u16(sum, vpadalq_u8(vpaddlq_u8(first), second));
            columns[0] = vaddw_u8(columns[0], vget_low_u8(first));
            columns[1] = vaddw_u8(columns[1], vget_high_u8(first));
            columns[2] = vaddw_u8(columns[2], vget_low_u8(second));
            columns[3] = vaddw_u8(columns[3], vget_high_u8(second));
        }
        let weights: [u16; 32] = std::array::from_fn(|index| 32 - index as u16);
        let mut weighted = vshlq_n_u32::<5>(before);
        for (column, weights) in columns.iter().zip(weights.as_chunks::<8>().0) {
            let weights = vld1q_u16(weights.as_ptr());
            weighted = vmlal_u16(weighted, vget_low_u16(*column), vget_low_u16(weights));
            weighted = vmlal_high_u16(weighted, *column, weights);
        }
        (vaddvq_u32(sum), vaddvq_u32(weighted))
    }
}

/// the same two sums with 256-bit registers
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn avx2(blocks: &[[u8; 32]]) -> (u32, u32) {
    use std::arch::x86_64::*;
    unsafe {
        let weights = _mm256_setr_epi8(
            32, 31, 30, 29, 28, 27, 26, 25, 24, 23, 22, 21, 20, 19, 18, 17, 16, 15, 14, 13, 12, 11,
            10, 9, 8, 7, 6, 5, 4, 3, 2, 1,
        );
        let zero = _mm256_setzero_si256();
        let mut before = zero;
        let mut sum = zero;
        let mut weighted = zero;
        for block in blocks {
            let bytes = _mm256_loadu_si256(block.as_ptr().cast());
            before = _mm256_add_epi32(before, sum);
            sum = _mm256_add_epi32(sum, _mm256_sad_epu8(bytes, zero));
            let products = _mm256_maddubs_epi16(bytes, weights);
            weighted =
                _mm256_add_epi32(weighted, _mm256_madd_epi16(products, _mm256_set1_epi16(1)));
        }
        weighted = _mm256_add_epi32(weighted, _mm256_slli_epi32::<5>(before));
        let total = |value: __m256i| {
            let halves = _mm_add_epi32(
                _mm256_castsi256_si128(value),
                _mm256_extracti128_si256::<1>(value),
            );
            let pairs = _mm_add_epi32(halves, _mm_shuffle_epi32::<0b0100_1110>(halves));
            _mm_cvtsi128_si32(_mm_add_epi32(
                pairs,
                _mm_shuffle_epi32::<0b1011_0001>(pairs),
            )) as u32
        };
        (total(sum), total(weighted))
    }
}
