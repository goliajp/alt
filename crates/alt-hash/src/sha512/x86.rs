//! SHA-512 compression for x86_64 with AVX2, BMI1 and BMI2: the message
//! schedule is computed four words at a time, and the rounds compile to
//! `rorx` and `andn`, which leave the flags alone.

use super::K;
use std::arch::x86_64::*;

macro_rules! rotate_right {
    ($x:expr, $n:literal) => {
        _mm256_or_si256(
            _mm256_srli_epi64::<$n>($x),
            _mm256_slli_epi64::<{ 64 - $n }>($x),
        )
    };
}

macro_rules! round {
    ($a:ident, $b:ident, $c:ident, $d:ident, $e:ident, $f:ident, $g:ident, $h:ident, $wk:expr) => {
        let t1 = $h
            .wrapping_add($e.rotate_right(14) ^ $e.rotate_right(18) ^ $e.rotate_right(41))
            .wrapping_add(($e & $f) ^ (!$e & $g))
            .wrapping_add($wk);
        let t2 = ($a.rotate_right(28) ^ $a.rotate_right(34) ^ $a.rotate_right(39))
            .wrapping_add(($a & $b) | ($c & ($a | $b)));
        $d = $d.wrapping_add(t1);
        $h = t1.wrapping_add(t2);
    };
}

/// words t-15 .. t-12 or t-7 .. t-4: the last three of `first` and the first of `second`
#[target_feature(enable = "avx2")]
fn shifted(first: __m256i, second: __m256i) -> __m256i {
    _mm256_permute4x64_epi64::<0b00_11_10_01>(_mm256_blend_epi32::<0b0000_0011>(first, second))
}

#[target_feature(enable = "avx2")]
fn small_sigma1(x: __m256i) -> __m256i {
    _mm256_xor_si256(
        _mm256_xor_si256(rotate_right!(x, 19), rotate_right!(x, 61)),
        _mm256_srli_epi64::<6>(x),
    )
}

#[target_feature(enable = "avx2,bmi1,bmi2")]
pub(super) unsafe fn compress(state: &mut [u64; 8], blocks: &[u8]) {
    let swap = _mm256_setr_epi8(
        7, 6, 5, 4, 3, 2, 1, 0, 15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0, 15, 14, 13,
        12, 11, 10, 9, 8,
    );
    for block in blocks.as_chunks::<128>().0 {
        // w[i] holds four consecutive schedule words; the sixteen-word window rolls through it
        let mut w = [_mm256_setzero_si256(); 4];
        for (i, slot) in w.iter_mut().enumerate() {
            // 32 bytes at offset 32 * i of the 128-byte block
            let words = unsafe { _mm256_loadu_si256(block.as_ptr().add(32 * i).cast()) };
            *slot = _mm256_shuffle_epi8(words, swap);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
        for group in 0..20 {
            let mut wk = [0u64; 4];
            // four words of the 80-word constant table and of the local array
            unsafe {
                let constants = _mm256_loadu_si256(K.as_ptr().add(4 * group).cast());
                let keyed = _mm256_add_epi64(w[group & 3], constants);
                _mm256_storeu_si256(wk.as_mut_ptr().cast(), keyed);
            }
            if group < 16 {
                let [y0, y1, y2, y3] = [0, 1, 2, 3].map(|i| w[(group + i) & 3]);
                let w15 = shifted(y0, y1);
                let sigma0 = _mm256_xor_si256(
                    _mm256_xor_si256(rotate_right!(w15, 1), rotate_right!(w15, 8)),
                    _mm256_srli_epi64::<7>(w15),
                );
                let partial = _mm256_add_epi64(_mm256_add_epi64(y0, sigma0), shifted(y2, y3));
                // words t-2 and t-1 exist already; the other two are the first half of this vector
                let sigma1 = small_sigma1(y3);
                let half =
                    _mm256_add_epi64(partial, _mm256_permute2x128_si256::<0x81>(sigma1, sigma1));
                let sigma1 = small_sigma1(half);
                w[group & 3] =
                    _mm256_add_epi64(half, _mm256_permute2x128_si256::<0x08>(sigma1, sigma1));
            }
            round!(a, b, c, d, e, f, g, h, wk[0]);
            round!(h, a, b, c, d, e, f, g, wk[1]);
            round!(g, h, a, b, c, d, e, f, wk[2]);
            round!(f, g, h, a, b, c, d, e, wk[3]);
            (a, b, c, d, e, f, g, h) = (e, f, g, h, a, b, c, d);
        }
        for (slot, value) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *slot = slot.wrapping_add(value);
        }
    }
}
