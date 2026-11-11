//! Four ChaCha20 blocks at a time: vector `i` holds state word `i` of each block.

use std::arch::aarch64::*;

macro_rules! rotate {
    ($x:expr, $n:literal) => {
        vsriq_n_u32::<{ 32 - $n }>(vshlq_n_u32::<$n>($x), $x)
    };
}

macro_rules! quarter {
    ($v:ident, $a:expr, $b:expr, $c:expr, $d:expr) => {
        $v[$a] = vaddq_u32($v[$a], $v[$b]);
        $v[$d] = vreinterpretq_u32_u16(vrev32q_u16(vreinterpretq_u16_u32(veorq_u32(
            $v[$d], $v[$a],
        ))));
        $v[$c] = vaddq_u32($v[$c], $v[$d]);
        let t = veorq_u32($v[$b], $v[$c]);
        $v[$b] = rotate!(t, 12);
        $v[$a] = vaddq_u32($v[$a], $v[$b]);
        let t = veorq_u32($v[$d], $v[$a]);
        $v[$d] = rotate!(t, 8);
        $v[$c] = vaddq_u32($v[$c], $v[$d]);
        let t = veorq_u32($v[$b], $v[$c]);
        $v[$b] = rotate!(t, 7);
    };
}

#[target_feature(enable = "neon")]
pub(super) unsafe fn apply(state: &mut [u32; 16], data: &mut [u8]) -> usize {
    let (groups, _) = data.as_chunks_mut::<256>();
    let handled = groups.len() * 256;
    let lanes = [0u32, 1, 2, 3];
    // loads read the four-word `lanes`; loads and stores of the text stay inside one 256-byte group
    unsafe {
        let step = vld1q_u32(lanes.as_ptr());
        for group in groups {
            let mut start = state.map(|word| vdupq_n_u32(word));
            start[12] = vaddq_u32(start[12], step);
            let mut v = start;
            for _ in 0..10 {
                quarter!(v, 0, 4, 8, 12);
                quarter!(v, 1, 5, 9, 13);
                quarter!(v, 2, 6, 10, 14);
                quarter!(v, 3, 7, 11, 15);
                quarter!(v, 0, 5, 10, 15);
                quarter!(v, 1, 6, 11, 12);
                quarter!(v, 2, 7, 8, 13);
                quarter!(v, 3, 4, 9, 14);
            }
            for (word, first) in v.iter_mut().zip(start) {
                *word = vaddq_u32(*word, first);
            }
            // turn four vectors of one word each into four vectors of one block each
            for quad in 0..4 {
                let [a, b, c, d] = [
                    v[4 * quad],
                    v[4 * quad + 1],
                    v[4 * quad + 2],
                    v[4 * quad + 3],
                ];
                let ab_low = vreinterpretq_u64_u32(vzip1q_u32(a, b));
                let ab_high = vreinterpretq_u64_u32(vzip2q_u32(a, b));
                let cd_low = vreinterpretq_u64_u32(vzip1q_u32(c, d));
                let cd_high = vreinterpretq_u64_u32(vzip2q_u32(c, d));
                let rows = [
                    vzip1q_u64(ab_low, cd_low),
                    vzip2q_u64(ab_low, cd_low),
                    vzip1q_u64(ab_high, cd_high),
                    vzip2q_u64(ab_high, cd_high),
                ];
                for (block, row) in rows.into_iter().enumerate() {
                    let at = group.as_mut_ptr().add(64 * block + 16 * quad);
                    vst1q_u8(at, veorq_u8(vld1q_u8(at), vreinterpretq_u8_u64(row)));
                }
            }
            state[12] = state[12].wrapping_add(4);
        }
    }
    handled
}
