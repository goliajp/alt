//! BLAKE2b compression with one 256-bit vector for each row of the state.

use super::{IV, SIGMA};
use std::arch::x86_64::*;

macro_rules! mix {
    ($a:ident, $b:ident, $c:ident, $d:ident, $x:expr, $y:expr, $r24:ident, $r16:ident) => {
        $a = _mm256_add_epi64(_mm256_add_epi64($a, $b), $x);
        $d = _mm256_shuffle_epi32::<0b10_11_00_01>(_mm256_xor_si256($d, $a));
        $c = _mm256_add_epi64($c, $d);
        $b = _mm256_shuffle_epi8(_mm256_xor_si256($b, $c), $r24);
        $a = _mm256_add_epi64(_mm256_add_epi64($a, $b), $y);
        $d = _mm256_shuffle_epi8(_mm256_xor_si256($d, $a), $r16);
        $c = _mm256_add_epi64($c, $d);
        let t = _mm256_xor_si256($b, $c);
        $b = _mm256_or_si256(_mm256_srli_epi64::<63>(t), _mm256_add_epi64(t, t));
    };
}

#[target_feature(enable = "avx2")]
pub(super) unsafe fn compress(state: &mut [u64; 8], block: &[u8; 128], bytes: u128, last: bool) {
    let mut m = [0i64; 16];
    for (slot, word) in m.iter_mut().zip(block.as_chunks::<8>().0) {
        *slot = i64::from_le_bytes(*word);
    }
    let words = |s: &[u8; 16], at: [usize; 4]| {
        let [w0, w1, w2, w3] = at.map(|i| m[s[i] as usize]);
        _mm256_setr_epi64x(w0, w1, w2, w3)
    };
    // byte shuffles that rotate each 64-bit lane right by 24 and by 16 bits
    let r24 = _mm256_setr_epi8(
        3, 4, 5, 6, 7, 0, 1, 2, 11, 12, 13, 14, 15, 8, 9, 10, 3, 4, 5, 6, 7, 0, 1, 2, 11, 12, 13,
        14, 15, 8, 9, 10,
    );
    let r16 = _mm256_setr_epi8(
        2, 3, 4, 5, 6, 7, 0, 1, 10, 11, 12, 13, 14, 15, 8, 9, 2, 3, 4, 5, 6, 7, 0, 1, 10, 11, 12,
        13, 14, 15, 8, 9,
    );
    // four words at the start and in the middle of the eight-word state and IV
    let (h0, h1, iv0, iv1) = unsafe {
        (
            _mm256_loadu_si256(state.as_ptr().cast()),
            _mm256_loadu_si256(state.as_ptr().add(4).cast()),
            _mm256_loadu_si256(IV.as_ptr().cast()),
            _mm256_loadu_si256(IV.as_ptr().add(4).cast()),
        )
    };
    let counter = _mm256_setr_epi64x(
        bytes as u64 as i64,
        (bytes >> 64) as u64 as i64,
        if last { -1 } else { 0 },
        0,
    );
    let (mut a, mut b, mut c, mut d) = (h0, h1, iv0, _mm256_xor_si256(iv1, counter));
    for s in &SIGMA {
        mix!(
            a,
            b,
            c,
            d,
            words(s, [0, 2, 4, 6]),
            words(s, [1, 3, 5, 7]),
            r24,
            r16
        );
        // rotate b, c and d by one, two and three words so the diagonals line up as columns
        b = _mm256_permute4x64_epi64::<0b00_11_10_01>(b);
        c = _mm256_permute4x64_epi64::<0b01_00_11_10>(c);
        d = _mm256_permute4x64_epi64::<0b10_01_00_11>(d);
        mix!(
            a,
            b,
            c,
            d,
            words(s, [8, 10, 12, 14]),
            words(s, [9, 11, 13, 15]),
            r24,
            r16
        );
        b = _mm256_permute4x64_epi64::<0b10_01_00_11>(b);
        c = _mm256_permute4x64_epi64::<0b01_00_11_10>(c);
        d = _mm256_permute4x64_epi64::<0b00_11_10_01>(d);
    }
    // the same two four-word halves of the state
    unsafe {
        _mm256_storeu_si256(
            state.as_mut_ptr().cast(),
            _mm256_xor_si256(h0, _mm256_xor_si256(a, c)),
        );
        _mm256_storeu_si256(
            state.as_mut_ptr().add(4).cast(),
            _mm256_xor_si256(h1, _mm256_xor_si256(b, d)),
        );
    }
}
