//! G with four 64-bit words per vector: vector `i` holds block words `4i` to `4i + 3`.

use super::Block;
use std::arch::x86_64::*;

/// a + b + 2 * low32(a) * low32(b) in each lane
macro_rules! multiply_add {
    ($a:expr, $b:expr) => {{
        let product = _mm256_mul_epu32($a, $b);
        _mm256_add_epi64(_mm256_add_epi64($a, $b), _mm256_add_epi64(product, product))
    }};
}

macro_rules! mix {
    ($a:ident, $b:ident, $c:ident, $d:ident, $r24:ident, $r16:ident) => {
        $a = multiply_add!($a, $b);
        $d = _mm256_shuffle_epi32::<0b10_11_00_01>(_mm256_xor_si256($d, $a));
        $c = multiply_add!($c, $d);
        $b = _mm256_shuffle_epi8(_mm256_xor_si256($b, $c), $r24);
        $a = multiply_add!($a, $b);
        $d = _mm256_shuffle_epi8(_mm256_xor_si256($d, $a), $r16);
        $c = multiply_add!($c, $d);
        let t = _mm256_xor_si256($b, $c);
        $b = _mm256_or_si256(_mm256_srli_epi64::<63>(t), _mm256_add_epi64(t, t));
    };
}

/// byte shuffles that rotate each 64-bit lane right by 24 and by 16 bits
#[target_feature(enable = "avx2")]
fn rotations() -> (__m256i, __m256i) {
    (
        _mm256_setr_epi8(
            3, 4, 5, 6, 7, 0, 1, 2, 11, 12, 13, 14, 15, 8, 9, 10, 3, 4, 5, 6, 7, 0, 1, 2, 11, 12,
            13, 14, 15, 8, 9, 10,
        ),
        _mm256_setr_epi8(
            2, 3, 4, 5, 6, 7, 0, 1, 10, 11, 12, 13, 14, 15, 8, 9, 2, 3, 4, 5, 6, 7, 0, 1, 10, 11,
            12, 13, 14, 15, 8, 9,
        ),
    )
}

/// the permutation P with one vector for each of the rows a, b, c, d
macro_rules! round {
    ($a:ident, $b:ident, $c:ident, $d:ident, $r24:ident, $r16:ident) => {
        mix!($a, $b, $c, $d, $r24, $r16);
        // rotate b, c and d by one, two and three words so the diagonals line up as columns
        $b = _mm256_permute4x64_epi64::<0b00_11_10_01>($b);
        $c = _mm256_permute4x64_epi64::<0b01_00_11_10>($c);
        $d = _mm256_permute4x64_epi64::<0b10_01_00_11>($d);
        mix!($a, $b, $c, $d, $r24, $r16);
        $b = _mm256_permute4x64_epi64::<0b10_01_00_11>($b);
        $c = _mm256_permute4x64_epi64::<0b01_00_11_10>($c);
        $d = _mm256_permute4x64_epi64::<0b00_11_10_01>($d);
    };
}

#[target_feature(enable = "avx2")]
pub(super) unsafe fn compress(previous: &Block, reference: &Block, out: &mut Block, xor: bool) {
    let (r24, r16) = rotations();
    let mut v = [_mm256_setzero_si256(); 32];
    for (i, slot) in v.iter_mut().enumerate() {
        // words 4i to 4i + 3 of a 128-word block, i below 32
        *slot = unsafe {
            _mm256_xor_si256(
                _mm256_loadu_si256(previous.as_ptr().add(4 * i).cast()),
                _mm256_loadu_si256(reference.as_ptr().add(4 * i).cast()),
            )
        };
    }
    let mixed = v;
    for row in v.as_chunks_mut::<4>().0 {
        let [mut a, mut b, mut c, mut d] = *row;
        round!(a, b, c, d, r24, r16);
        *row = [a, b, c, d];
    }
    // column c is words 2c and 2c + 1 of every row: for the row pairs (0, 1), (2, 3),
    // (4, 5), (6, 7) that is one 128-bit half of vector c / 2 in each row of the pair
    for k in 0..4 {
        let mut halves = [[_mm256_setzero_si256(); 4]; 2];
        for m in 0..4 {
            let (upper, lower) = (v[8 * m + k], v[8 * m + 4 + k]);
            halves[0][m] = _mm256_permute2x128_si256::<0x20>(upper, lower);
            halves[1][m] = _mm256_permute2x128_si256::<0x31>(upper, lower);
        }
        for half in &mut halves {
            let [mut a, mut b, mut c, mut d] = *half;
            round!(a, b, c, d, r24, r16);
            *half = [a, b, c, d];
        }
        for m in 0..4 {
            v[8 * m + k] = _mm256_permute2x128_si256::<0x20>(halves[0][m], halves[1][m]);
            v[8 * m + 4 + k] = _mm256_permute2x128_si256::<0x31>(halves[0][m], halves[1][m]);
        }
    }
    for i in 0..32 {
        // words 4i to 4i + 3 of a 128-word block, i below 32
        unsafe {
            let at = out.as_mut_ptr().add(4 * i).cast();
            let mut word = _mm256_xor_si256(v[i], mixed[i]);
            if xor {
                word = _mm256_xor_si256(word, _mm256_loadu_si256(at));
            }
            _mm256_storeu_si256(at, word);
        }
    }
}
