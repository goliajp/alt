//! SHA-256 compression with the x86 SHA instructions.

use super::super::K;
use std::arch::x86_64::*;

/// Four rounds on the group `$m0`. The schedule steps prepare the following groups:
/// `$m1` is the next one, `$m3` the one before `$m0`.
macro_rules! group {
    ($first:ident, $second:ident, $at:literal, $m0:ident, $m1:ident, $m2:ident, $m3:ident, $step:tt) => {
        let words = _mm_add_epi32($m0, _mm_loadu_si128(K.as_ptr().add($at * 4).cast()));
        $second = _mm_sha256rnds2_epu32($second, $first, words);
        finish!($step, $m0, $m1, $m3);
        $first = _mm_sha256rnds2_epu32($first, $second, _mm_shuffle_epi32::<0x0e>(words));
        begin!($step, $m0, $m3);
    };
}

/// completes the next group, which needs the last word of this one
macro_rules! finish {
    (none, $m0:ident, $m1:ident, $m3:ident) => {};
    (begin, $m0:ident, $m1:ident, $m3:ident) => {};
    ($step:tt, $m0:ident, $m1:ident, $m3:ident) => {
        $m1 = _mm_sha256msg2_epu32(_mm_add_epi32($m1, _mm_alignr_epi8::<4>($m0, $m3)), $m0);
    };
}

/// starts the group three ahead, in the register of the one before this
macro_rules! begin {
    (none, $m0:ident, $m3:ident) => {};
    (finish, $m0:ident, $m3:ident) => {};
    ($step:tt, $m0:ident, $m3:ident) => {
        $m3 = _mm_sha256msg1_epu32($m3, $m0);
    };
}

/// folds whole 64-byte blocks into `state`
#[target_feature(enable = "sha,sse2,ssse3,sse4.1")]
pub(in super::super) unsafe fn compress(state: &mut [u32; 8], blocks: &[u8]) {
    unsafe {
        let swap = _mm_set_epi64x(0x0c0d_0e0f_0809_0a0b, 0x0405_0607_0001_0203);
        // the instructions want the state as (a, b, e, f) and (c, d, g, h)
        let low = _mm_shuffle_epi32::<0xb1>(_mm_loadu_si128(state.as_ptr().cast()));
        let high = _mm_shuffle_epi32::<0x1b>(_mm_loadu_si128(state.as_ptr().add(4).cast()));
        let mut abef = _mm_alignr_epi8::<8>(low, high);
        let mut cdgh = _mm_blend_epi16::<0xf0>(high, low);
        for block in blocks.as_chunks::<64>().0 {
            let load =
                |at: usize| _mm_shuffle_epi8(_mm_loadu_si128(block.as_ptr().add(at).cast()), swap);
            let (mut m0, mut m1, mut m2, mut m3) = (load(0), load(16), load(32), load(48));
            let before = (abef, cdgh);
            group!(abef, cdgh, 0, m0, m1, m2, m3, none);
            group!(abef, cdgh, 1, m1, m2, m3, m0, begin);
            group!(abef, cdgh, 2, m2, m3, m0, m1, begin);
            group!(abef, cdgh, 3, m3, m0, m1, m2, all);
            group!(abef, cdgh, 4, m0, m1, m2, m3, all);
            group!(abef, cdgh, 5, m1, m2, m3, m0, all);
            group!(abef, cdgh, 6, m2, m3, m0, m1, all);
            group!(abef, cdgh, 7, m3, m0, m1, m2, all);
            group!(abef, cdgh, 8, m0, m1, m2, m3, all);
            group!(abef, cdgh, 9, m1, m2, m3, m0, all);
            group!(abef, cdgh, 10, m2, m3, m0, m1, all);
            group!(abef, cdgh, 11, m3, m0, m1, m2, all);
            group!(abef, cdgh, 12, m0, m1, m2, m3, all);
            group!(abef, cdgh, 13, m1, m2, m3, m0, finish);
            group!(abef, cdgh, 14, m2, m3, m0, m1, finish);
            group!(abef, cdgh, 15, m3, m0, m1, m2, none);
            abef = _mm_add_epi32(abef, before.0);
            cdgh = _mm_add_epi32(cdgh, before.1);
        }
        let low = _mm_shuffle_epi32::<0x1b>(abef);
        let high = _mm_shuffle_epi32::<0xb1>(cdgh);
        _mm_storeu_si128(
            state.as_mut_ptr().cast(),
            _mm_blend_epi16::<0xf0>(low, high),
        );
        _mm_storeu_si128(
            state.as_mut_ptr().add(4).cast(),
            _mm_alignr_epi8::<8>(high, low),
        );
    }
}
