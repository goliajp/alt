//! G with two 64-bit words per vector: vector `i` holds block words `2i` and `2i + 1`.

use super::Block;
use std::arch::aarch64::*;

/// a + b + 2 * low32(a) * low32(b) in each lane
macro_rules! multiply_add {
    ($a:expr, $b:expr) => {{
        let product = vmull_u32(vmovn_u64($a), vmovn_u64($b));
        vaddq_u64(vaddq_u64($a, $b), vaddq_u64(product, product))
    }};
}

macro_rules! rotate_right {
    ($x:expr, $n:literal) => {{
        let x = $x;
        vsriq_n_u64::<$n>(vshlq_n_u64::<{ 64 - $n }>(x), x)
    }};
}

macro_rules! mix {
    ($a:ident, $b:ident, $c:ident, $d:ident) => {
        $a = multiply_add!($a, $b);
        $d = vreinterpretq_u64_u32(vrev64q_u32(vreinterpretq_u32_u64(veorq_u64($d, $a))));
        $c = multiply_add!($c, $d);
        $b = rotate_right!(veorq_u64($b, $c), 24);
        $a = multiply_add!($a, $b);
        $d = rotate_right!(veorq_u64($d, $a), 16);
        $c = multiply_add!($c, $d);
        $b = rotate_right!(veorq_u64($b, $c), 63);
    };
}

/// the permutation P over eight vectors: two for each of the rows a, b, c, d
macro_rules! round {
    ($v:ident, $($i:expr),+) => {{
        let [mut a0, mut a1, mut b0, mut b1, mut c0, mut c1, mut d0, mut d1] = [$($v[$i]),+];
        mix!(a0, b0, c0, d0);
        mix!(a1, b1, c1, d1);
        // rotate b, c and d by one, two and three words so the diagonals line up as columns
        (b0, b1) = (vextq_u64::<1>(b0, b1), vextq_u64::<1>(b1, b0));
        (c0, c1) = (c1, c0);
        (d0, d1) = (vextq_u64::<1>(d1, d0), vextq_u64::<1>(d0, d1));
        mix!(a0, b0, c0, d0);
        mix!(a1, b1, c1, d1);
        (b0, b1) = (vextq_u64::<1>(b1, b0), vextq_u64::<1>(b0, b1));
        (c0, c1) = (c1, c0);
        (d0, d1) = (vextq_u64::<1>(d0, d1), vextq_u64::<1>(d1, d0));
        for (at, vector) in [$($i),+].into_iter().zip([a0, a1, b0, b1, c0, c1, d0, d1]) {
            $v[at] = vector;
        }
    }};
}

#[target_feature(enable = "neon")]
pub(super) unsafe fn compress(previous: &Block, reference: &Block, out: &mut Block, xor: bool) {
    // every load and store covers words 2i and 2i + 1 of a 128-word block, i below 64
    unsafe {
        let mut v = [vdupq_n_u64(0); 64];
        for (i, slot) in v.iter_mut().enumerate() {
            *slot = veorq_u64(
                vld1q_u64(previous.as_ptr().add(2 * i)),
                vld1q_u64(reference.as_ptr().add(2 * i)),
            );
        }
        let mixed = v;
        for row in 0..8 {
            let r = 8 * row;
            round!(v, r, r + 1, r + 2, r + 3, r + 4, r + 5, r + 6, r + 7);
        }
        for c in 0..8 {
            round!(v, c, c + 8, c + 16, c + 24, c + 32, c + 40, c + 48, c + 56);
        }
        for i in 0..64 {
            let at = out.as_mut_ptr().add(2 * i);
            let mut word = veorq_u64(v[i], mixed[i]);
            if xor {
                word = veorq_u64(word, vld1q_u64(at));
            }
            vst1q_u64(at, word);
        }
    }
}
