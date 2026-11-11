//! Eight ChaCha20 blocks at a time: vector `i` holds state word `i` of each block.

use std::arch::x86_64::*;

macro_rules! rotate {
    ($x:expr, $n:literal) => {
        _mm256_or_si256(
            _mm256_slli_epi32::<$n>($x),
            _mm256_srli_epi32::<{ 32 - $n }>($x),
        )
    };
}

macro_rules! quarter {
    ($v:ident, $rot16:ident, $rot8:ident, $a:expr, $b:expr, $c:expr, $d:expr) => {
        $v[$a] = _mm256_add_epi32($v[$a], $v[$b]);
        $v[$d] = _mm256_shuffle_epi8(_mm256_xor_si256($v[$d], $v[$a]), $rot16);
        $v[$c] = _mm256_add_epi32($v[$c], $v[$d]);
        let t = _mm256_xor_si256($v[$b], $v[$c]);
        $v[$b] = rotate!(t, 12);
        $v[$a] = _mm256_add_epi32($v[$a], $v[$b]);
        $v[$d] = _mm256_shuffle_epi8(_mm256_xor_si256($v[$d], $v[$a]), $rot8);
        $v[$c] = _mm256_add_epi32($v[$c], $v[$d]);
        let t = _mm256_xor_si256($v[$b], $v[$c]);
        $v[$b] = rotate!(t, 7);
    };
}

#[target_feature(enable = "avx2")]
pub(super) unsafe fn apply(state: &mut [u32; 16], data: &mut [u8]) -> usize {
    let (groups, _) = data.as_chunks_mut::<512>();
    let handled = groups.len() * 512;
    let rot16 = _mm256_set_epi8(
        13, 12, 15, 14, 9, 8, 11, 10, 5, 4, 7, 6, 1, 0, 3, 2, 13, 12, 15, 14, 9, 8, 11, 10, 5, 4,
        7, 6, 1, 0, 3, 2,
    );
    let rot8 = _mm256_set_epi8(
        14, 13, 12, 15, 10, 9, 8, 11, 6, 5, 4, 7, 2, 1, 0, 3, 14, 13, 12, 15, 10, 9, 8, 11, 6, 5,
        4, 7, 2, 1, 0, 3,
    );
    let step = _mm256_set_epi32(7, 6, 5, 4, 3, 2, 1, 0);
    for group in groups {
        let mut start = state.map(|word| _mm256_set1_epi32(word as i32));
        start[12] = _mm256_add_epi32(start[12], step);
        let mut v = start;
        for _ in 0..10 {
            quarter!(v, rot16, rot8, 0, 4, 8, 12);
            quarter!(v, rot16, rot8, 1, 5, 9, 13);
            quarter!(v, rot16, rot8, 2, 6, 10, 14);
            quarter!(v, rot16, rot8, 3, 7, 11, 15);
            quarter!(v, rot16, rot8, 0, 5, 10, 15);
            quarter!(v, rot16, rot8, 1, 6, 11, 12);
            quarter!(v, rot16, rot8, 2, 7, 8, 13);
            quarter!(v, rot16, rot8, 3, 4, 9, 14);
        }
        for (word, first) in v.iter_mut().zip(start) {
            *word = _mm256_add_epi32(*word, first);
        }
        // turn eight vectors of one word each into eight vectors of half a block each
        for half in 0..2 {
            let w = &v[8 * half..8 * half + 8];
            let pairs = [
                _mm256_unpacklo_epi32(w[0], w[1]),
                _mm256_unpackhi_epi32(w[0], w[1]),
                _mm256_unpacklo_epi32(w[2], w[3]),
                _mm256_unpackhi_epi32(w[2], w[3]),
                _mm256_unpacklo_epi32(w[4], w[5]),
                _mm256_unpackhi_epi32(w[4], w[5]),
                _mm256_unpacklo_epi32(w[6], w[7]),
                _mm256_unpackhi_epi32(w[6], w[7]),
            ];
            // quads[i] and quads[i + 4] hold words 0-3 and 4-7 of blocks i and i + 4
            let quads = [
                _mm256_unpacklo_epi64(pairs[0], pairs[2]),
                _mm256_unpackhi_epi64(pairs[0], pairs[2]),
                _mm256_unpacklo_epi64(pairs[1], pairs[3]),
                _mm256_unpackhi_epi64(pairs[1], pairs[3]),
                _mm256_unpacklo_epi64(pairs[4], pairs[6]),
                _mm256_unpackhi_epi64(pairs[4], pairs[6]),
                _mm256_unpacklo_epi64(pairs[5], pairs[7]),
                _mm256_unpackhi_epi64(pairs[5], pairs[7]),
            ];
            for i in 0..4 {
                let low = _mm256_permute2x128_si256::<0x20>(quads[i], quads[i + 4]);
                let high = _mm256_permute2x128_si256::<0x31>(quads[i], quads[i + 4]);
                for (block, row) in [(i, low), (i + 4, high)] {
                    // the 32 bytes at this offset lie inside the 512-byte group
                    unsafe {
                        let at = group.as_mut_ptr().add(64 * block + 32 * half).cast();
                        _mm256_storeu_si256(at, _mm256_xor_si256(_mm256_loadu_si256(at), row));
                    }
                }
            }
        }
        state[12] = state[12].wrapping_add(8);
    }
    handled
}
