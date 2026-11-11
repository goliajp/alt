//! SHA-512 compression for arm64 cores without the SHA-512 instructions: the
//! message schedule is computed two words at a time in NEON vectors, which run
//! beside the rounds; the rounds themselves are integer code.

use super::K;
use std::arch::aarch64::*;

macro_rules! rotate_right {
    ($x:expr, $n:literal) => {
        vsriq_n_u64::<$n>(vshlq_n_u64::<{ 64 - $n }>($x), $x)
    };
}

macro_rules! round {
    ($a:ident, $b:ident, $c:ident, $d:ident, $e:ident, $f:ident, $g:ident, $h:ident, $wk:expr) => {
        let t1 = $h
            .wrapping_add($e.rotate_right(14) ^ $e.rotate_right(18) ^ $e.rotate_right(41))
            .wrapping_add($g ^ ($e & ($f ^ $g)))
            .wrapping_add($wk);
        let t2 = ($a.rotate_right(28) ^ $a.rotate_right(34) ^ $a.rotate_right(39))
            .wrapping_add(($a & $b) | ($c & ($a | $b)));
        $d = $d.wrapping_add(t1);
        $h = t1.wrapping_add(t2);
    };
}

/// `$w0` becomes the two schedule words sixteen further on; the last sixteen rounds
/// need none
macro_rules! advance {
    (true, $w0:ident, $w1:ident, $w4:ident, $w5:ident, $w7:ident) => {
        // words t-15 and t-14, then t-7 and t-6; t-2 and t-1 are `$w7` as it is
        let w15 = vextq_u64::<1>($w0, $w1);
        let sigma0 = veorq_u64(
            veorq_u64(rotate_right!(w15, 1), rotate_right!(w15, 8)),
            vshrq_n_u64::<7>(w15),
        );
        let sigma1 = veorq_u64(
            veorq_u64(rotate_right!($w7, 19), rotate_right!($w7, 61)),
            vshrq_n_u64::<6>($w7),
        );
        $w0 = vaddq_u64(
            vaddq_u64($w0, sigma0),
            vaddq_u64(vextq_u64::<1>($w4, $w5), sigma1),
        );
    };
    (false, $w0:ident, $w1:ident, $w4:ident, $w5:ident, $w7:ident) => {};
}

/// two rounds on the words in `$w0`
macro_rules! two {
    ($a:ident, $b:ident, $c:ident, $d:ident, $e:ident, $f:ident, $g:ident, $h:ident,
     $w0:ident, $w1:ident, $w4:ident, $w5:ident, $w7:ident, $k:expr, $schedule:tt) => {
        let mut wk = [0u64; 2];
        vst1q_u64(
            wk.as_mut_ptr(),
            vaddq_u64($w0, vld1q_u64(K.as_ptr().add($k))),
        );
        advance!($schedule, $w0, $w1, $w4, $w5, $w7);
        round!($a, $b, $c, $d, $e, $f, $g, $h, wk[0]);
        round!($h, $a, $b, $c, $d, $e, $f, $g, wk[1]);
    };
}

#[target_feature(enable = "neon")]
pub(super) unsafe fn compress(state: &mut [u64; 8], blocks: &[u8]) {
    for block in blocks.as_chunks::<128>().0 {
        // loads and stores stay inside `K`, the 128-byte block and the local arrays
        unsafe {
            let word =
                |i: usize| vreinterpretq_u64_u8(vrev64q_u8(vld1q_u8(block.as_ptr().add(16 * i))));
            // two consecutive schedule words each; the sixteen-word window rolls through them
            let (mut w0, mut w1, mut w2, mut w3) = (word(0), word(1), word(2), word(3));
            let (mut w4, mut w5, mut w6, mut w7) = (word(4), word(5), word(6), word(7));
            let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
            // Sixteen rounds, after which every name holds what it stands for again:
            // written out so that no state word moves between registers.
            macro_rules! sixteen {
                ($k:expr, $schedule:tt) => {
                    two!(
                        a,
                        b,
                        c,
                        d,
                        e,
                        f,
                        g,
                        h,
                        w0,
                        w1,
                        w4,
                        w5,
                        w7,
                        $k + 0,
                        $schedule
                    );
                    two!(
                        g,
                        h,
                        a,
                        b,
                        c,
                        d,
                        e,
                        f,
                        w1,
                        w2,
                        w5,
                        w6,
                        w0,
                        $k + 2,
                        $schedule
                    );
                    two!(
                        e,
                        f,
                        g,
                        h,
                        a,
                        b,
                        c,
                        d,
                        w2,
                        w3,
                        w6,
                        w7,
                        w1,
                        $k + 4,
                        $schedule
                    );
                    two!(
                        c,
                        d,
                        e,
                        f,
                        g,
                        h,
                        a,
                        b,
                        w3,
                        w4,
                        w7,
                        w0,
                        w2,
                        $k + 6,
                        $schedule
                    );
                    two!(
                        a,
                        b,
                        c,
                        d,
                        e,
                        f,
                        g,
                        h,
                        w4,
                        w5,
                        w0,
                        w1,
                        w3,
                        $k + 8,
                        $schedule
                    );
                    two!(
                        g,
                        h,
                        a,
                        b,
                        c,
                        d,
                        e,
                        f,
                        w5,
                        w6,
                        w1,
                        w2,
                        w4,
                        $k + 10,
                        $schedule
                    );
                    two!(
                        e,
                        f,
                        g,
                        h,
                        a,
                        b,
                        c,
                        d,
                        w6,
                        w7,
                        w2,
                        w3,
                        w5,
                        $k + 12,
                        $schedule
                    );
                    two!(
                        c,
                        d,
                        e,
                        f,
                        g,
                        h,
                        a,
                        b,
                        w7,
                        w0,
                        w3,
                        w4,
                        w6,
                        $k + 14,
                        $schedule
                    );
                };
            }
            for group in 0..4 {
                sixteen!(16 * group, true);
            }
            sixteen!(64, false);
            for (slot, value) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
                *slot = slot.wrapping_add(value);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{INITIAL, portable};

    /// on a CPU with the SHA-512 instructions nothing else runs this implementation
    #[test]
    fn matches_portable_over_any_number_of_blocks() {
        if !std::arch::is_aarch64_feature_detected!("neon") {
            return;
        }
        let mut random = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move || {
            random ^= random << 13;
            random ^= random >> 7;
            random ^= random << 17;
            random
        };
        for blocks in (0..=9).chain([64, 257]) {
            for _ in 0..50 {
                let data: Vec<u8> = (0..blocks * 128).map(|_| next() as u8).collect();
                let mut start = INITIAL;
                start.iter_mut().for_each(|word| *word ^= next());
                let (mut expected, mut state) = (start, start);
                portable(&mut expected, &data);
                // neon was detected above
                unsafe { super::compress(&mut state, &data) };
                assert_eq!(state, expected, "{blocks} blocks");
            }
        }
        // all-ones and all-zeros words carry through every addition and rotation
        for fill in [0u8, 0xff] {
            let data = vec![fill; 3 * 128];
            let (mut expected, mut state) = ([u64::MAX; 8], [u64::MAX; 8]);
            portable(&mut expected, &data);
            // neon was detected above
            unsafe { super::compress(&mut state, &data) };
            assert_eq!(state, expected);
        }
    }
}
