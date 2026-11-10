//! SHA-1 compression with the x86 SHA instructions.

use std::arch::x86_64::*;

/// Four rounds. `$e` carries the fifth working variable into them and `$next` keeps
/// the state they started from, which becomes the fifth variable four rounds on. The
/// three schedule steps prepare the message words of the following three groups.
macro_rules! group {
    ($abcd:ident, $e:ident, $next:ident, $function:literal, $m0:ident, $m1:ident, $m2:ident, $m3:ident, $step:tt) => {
        $e = _mm_sha1nexte_epu32($e, $m0);
        $next = $abcd;
        schedule!($step, $m0, $m1, $m2, $m3);
        $abcd = _mm_sha1rnds4_epu32::<$function>($abcd, $e);
    };
}

/// `$m0` is the group being consumed; `$m1`, `$m2`, `$m3` are the next three in order
macro_rules! schedule {
    (none, $m0:ident, $m1:ident, $m2:ident, $m3:ident) => {};
    (first, $m0:ident, $m1:ident, $m2:ident, $m3:ident) => {
        $m3 = _mm_sha1msg1_epu32($m3, $m0);
    };
    (second, $m0:ident, $m1:ident, $m2:ident, $m3:ident) => {
        $m3 = _mm_sha1msg1_epu32($m3, $m0);
        $m2 = _mm_xor_si128($m2, $m0);
    };
    (all, $m0:ident, $m1:ident, $m2:ident, $m3:ident) => {
        $m1 = _mm_sha1msg2_epu32($m1, $m0);
        $m3 = _mm_sha1msg1_epu32($m3, $m0);
        $m2 = _mm_xor_si128($m2, $m0);
    };
    (last_two, $m0:ident, $m1:ident, $m2:ident, $m3:ident) => {
        $m1 = _mm_sha1msg2_epu32($m1, $m0);
        $m2 = _mm_xor_si128($m2, $m0);
    };
    (last, $m0:ident, $m1:ident, $m2:ident, $m3:ident) => {
        $m1 = _mm_sha1msg2_epu32($m1, $m0);
    };
}

/// folds whole 64-byte blocks into `state`
#[target_feature(enable = "sha,sse2,ssse3,sse4.1")]
pub(in super::super) unsafe fn compress(state: &mut [u32; 5], blocks: &[u8]) {
    unsafe {
        let reverse = _mm_set_epi64x(0x0001_0203_0405_0607, 0x0809_0a0b_0c0d_0e0f);
        let mut abcd = _mm_shuffle_epi32::<0x1b>(_mm_loadu_si128(state.as_ptr().cast()));
        let mut e0 = _mm_set_epi32(state[4] as i32, 0, 0, 0);
        let mut e1;
        for block in blocks.as_chunks::<64>().0 {
            let load = |at: usize| {
                _mm_shuffle_epi8(_mm_loadu_si128(block.as_ptr().add(at).cast()), reverse)
            };
            let (mut m0, mut m1, mut m2, mut m3) = (load(0), load(16), load(32), load(48));
            let (abcd_before, e_before) = (abcd, e0);
            e0 = _mm_add_epi32(e0, m0);
            e1 = abcd;
            abcd = _mm_sha1rnds4_epu32::<0>(abcd, e0);
            group!(abcd, e1, e0, 0, m1, m2, m3, m0, first);
            group!(abcd, e0, e1, 0, m2, m3, m0, m1, second);
            group!(abcd, e1, e0, 0, m3, m0, m1, m2, all);
            group!(abcd, e0, e1, 0, m0, m1, m2, m3, all);
            group!(abcd, e1, e0, 1, m1, m2, m3, m0, all);
            group!(abcd, e0, e1, 1, m2, m3, m0, m1, all);
            group!(abcd, e1, e0, 1, m3, m0, m1, m2, all);
            group!(abcd, e0, e1, 1, m0, m1, m2, m3, all);
            group!(abcd, e1, e0, 1, m1, m2, m3, m0, all);
            group!(abcd, e0, e1, 2, m2, m3, m0, m1, all);
            group!(abcd, e1, e0, 2, m3, m0, m1, m2, all);
            group!(abcd, e0, e1, 2, m0, m1, m2, m3, all);
            group!(abcd, e1, e0, 2, m1, m2, m3, m0, all);
            group!(abcd, e0, e1, 2, m2, m3, m0, m1, all);
            group!(abcd, e1, e0, 3, m3, m0, m1, m2, all);
            group!(abcd, e0, e1, 3, m0, m1, m2, m3, all);
            group!(abcd, e1, e0, 3, m1, m2, m3, m0, last_two);
            group!(abcd, e0, e1, 3, m2, m3, m0, m1, last);
            group!(abcd, e1, e0, 3, m3, m0, m1, m2, none);
            e0 = _mm_sha1nexte_epu32(e0, e_before);
            abcd = _mm_add_epi32(abcd, abcd_before);
        }
        _mm_storeu_si128(state.as_mut_ptr().cast(), _mm_shuffle_epi32::<0x1b>(abcd));
        state[4] = _mm_extract_epi32::<3>(e0) as u32;
    }
}
