//! SHA-1 compression for x86-64 without SHA instructions.

use super::K;
use std::arch::x86_64::*;

/// the round function of (b, c, d) into r9d
macro_rules! function_asm {
    (choose, $b:literal, $c:literal, $d:literal) => {
        concat!(
            "mov r9d, ",
            $c,
            "\n",
            "xor r9d, ",
            $d,
            "\n",
            "and r9d, ",
            $b,
            "\n",
            "xor r9d, ",
            $d,
            "\n"
        )
    };
    (parity, $b:literal, $c:literal, $d:literal) => {
        concat!(
            "mov r9d, ",
            $b,
            "\n",
            "xor r9d, ",
            $c,
            "\n",
            "xor r9d, ",
            $d,
            "\n"
        )
    };
    (majority, $b:literal, $c:literal, $d:literal) => {
        concat!(
            "mov r9d, ",
            $b,
            "\n",
            "or r9d, ",
            $c,
            "\n",
            "and r9d, ",
            $d,
            "\n",
            "mov r10d, ",
            $b,
            "\n",
            "and r10d, ",
            $c,
            "\n",
            "or r9d, r10d\n"
        )
    };
}

/// One round. The word (constant included) and the round function are added to `e`
/// first; the rotation of `a`, which is the value computed last, joins at the end.
macro_rules! round_asm {
    ($f:ident, $a:literal, $b:literal, $c:literal, $d:literal, $e:literal, $t:literal) => {
        concat!(
            function_asm!($f, $b, $c, $d),
            "add ",
            $e,
            ", dword ptr [r11 + ",
            $t,
            "*4]\n",
            "add ",
            $e,
            ", r9d\n",
            "mov r10d, ",
            $a,
            "\n",
            "rol r10d, 5\n",
            "add ",
            $e,
            ", r10d\n",
            "rol ",
            $b,
            ", 30\n"
        )
    };
}

/// The same round with BMI2 rotations, which need no copy and leave the flags alone.
macro_rules! round_bmi {
    ($f:ident, $a:literal, $b:literal, $c:literal, $d:literal, $e:literal, $t:literal) => {
        concat!(
            function_asm!($f, $b, $c, $d),
            "add ",
            $e,
            ", dword ptr [r11 + ",
            $t,
            "*4]\n",
            "add ",
            $e,
            ", r9d\n",
            "rorx r10d, ",
            $a,
            ", 27\n",
            "add ",
            $e,
            ", r10d\n",
            "rorx ",
            $b,
            ", ",
            $b,
            ", 2\n"
        )
    };
}

/// The next four schedule words. `$w0` holds the words sixteen back and is replaced by
/// the new ones; `$w1`..`$w3` are the three groups after it.
macro_rules! extend {
    ($wk:ident, $group:literal, $k:expr, $w0:ident, $w1:ident, $w2:ident, $w3:ident) => {
        let mixed = _mm_xor_si128(
            _mm_xor_si128($w0, $w2),
            _mm_xor_si128(_mm_srli_si128::<4>($w3), _mm_alignr_epi8::<8>($w1, $w0)),
        );
        let rotated = _mm_or_si128(_mm_slli_epi32::<1>(mixed), _mm_srli_epi32::<31>(mixed));
        // the last word also depends on the first word of this same group
        let carry = _mm_slli_si128::<12>(mixed);
        $w0 = _mm_xor_si128(
            rotated,
            _mm_or_si128(_mm_slli_epi32::<2>(carry), _mm_srli_epi32::<30>(carry)),
        );
        _mm_storeu_si128($wk.add($group * 4).cast(), _mm_add_epi32($w0, $k));
    };
}

/// The block function, instantiated once per instruction-set level. The message
/// schedule is computed four words at a time with the round constant already added,
/// so the eighty rounds only read finished words.
macro_rules! block_function {
    ($(#[$attribute:meta])* $name:ident, $round:ident) => {
        $(#[$attribute])*
        pub(super) unsafe fn $name(state: &mut [u32; 5], blocks: &[u8]) {
    unsafe {
        let swap = _mm_set_epi8(12, 13, 14, 15, 8, 9, 10, 11, 4, 5, 6, 7, 0, 1, 2, 3);
        let k = K.map(|value| _mm_set1_epi32(value as i32));
        for block in blocks.as_chunks::<64>().0 {
            // every word is written by the schedule below before the rounds read it
            let mut words = std::mem::MaybeUninit::<[u32; 80]>::uninit();
            let wk = words.as_mut_ptr().cast::<u32>();
            let mut w0 = _mm_shuffle_epi8(_mm_loadu_si128(block.as_ptr().cast()), swap);
            let mut w1 = _mm_shuffle_epi8(_mm_loadu_si128(block.as_ptr().add(16).cast()), swap);
            let mut w2 = _mm_shuffle_epi8(_mm_loadu_si128(block.as_ptr().add(32).cast()), swap);
            let mut w3 = _mm_shuffle_epi8(_mm_loadu_si128(block.as_ptr().add(48).cast()), swap);
            _mm_storeu_si128(wk.cast(), _mm_add_epi32(w0, k[0]));
            _mm_storeu_si128(wk.add(4).cast(), _mm_add_epi32(w1, k[0]));
            _mm_storeu_si128(wk.add(8).cast(), _mm_add_epi32(w2, k[0]));
            _mm_storeu_si128(wk.add(12).cast(), _mm_add_epi32(w3, k[0]));
            // the four schedule registers stay in named variables: an indexed array would
            // put them on the stack and chain every group through a store and a reload
            extend!(wk, 4, k[0], w0, w1, w2, w3);
            extend!(wk, 5, k[1], w1, w2, w3, w0);
            extend!(wk, 6, k[1], w2, w3, w0, w1);
            extend!(wk, 7, k[1], w3, w0, w1, w2);
            extend!(wk, 8, k[1], w0, w1, w2, w3);
            extend!(wk, 9, k[1], w1, w2, w3, w0);
            extend!(wk, 10, k[2], w2, w3, w0, w1);
            extend!(wk, 11, k[2], w3, w0, w1, w2);
            extend!(wk, 12, k[2], w0, w1, w2, w3);
            extend!(wk, 13, k[2], w1, w2, w3, w0);
            extend!(wk, 14, k[2], w2, w3, w0, w1);
            extend!(wk, 15, k[3], w3, w0, w1, w2);
            extend!(wk, 16, k[3], w0, w1, w2, w3);
            extend!(wk, 17, k[3], w1, w2, w3, w0);
            extend!(wk, 18, k[3], w2, w3, w0, w1);
            extend!(wk, 19, k[3], w3, w0, w1, w2);
            let [mut a, mut b, mut c, mut d, mut e] = *state;
            // The rounds are assembly: the compiler reassociates the additions of a
            // round and puts the newest value at the head of the chain, which doubles
            // the latency of every round.
            std::arch::asm!(
                $round!(choose, "eax", "edx", "esi", "edi", "r8d", 0),
                $round!(choose, "r8d", "eax", "edx", "esi", "edi", 1),
                $round!(choose, "edi", "r8d", "eax", "edx", "esi", 2),
                $round!(choose, "esi", "edi", "r8d", "eax", "edx", 3),
                $round!(choose, "edx", "esi", "edi", "r8d", "eax", 4),
                $round!(choose, "eax", "edx", "esi", "edi", "r8d", 5),
                $round!(choose, "r8d", "eax", "edx", "esi", "edi", 6),
                $round!(choose, "edi", "r8d", "eax", "edx", "esi", 7),
                $round!(choose, "esi", "edi", "r8d", "eax", "edx", 8),
                $round!(choose, "edx", "esi", "edi", "r8d", "eax", 9),
                $round!(choose, "eax", "edx", "esi", "edi", "r8d", 10),
                $round!(choose, "r8d", "eax", "edx", "esi", "edi", 11),
                $round!(choose, "edi", "r8d", "eax", "edx", "esi", 12),
                $round!(choose, "esi", "edi", "r8d", "eax", "edx", 13),
                $round!(choose, "edx", "esi", "edi", "r8d", "eax", 14),
                $round!(choose, "eax", "edx", "esi", "edi", "r8d", 15),
                $round!(choose, "r8d", "eax", "edx", "esi", "edi", 16),
                $round!(choose, "edi", "r8d", "eax", "edx", "esi", 17),
                $round!(choose, "esi", "edi", "r8d", "eax", "edx", 18),
                $round!(choose, "edx", "esi", "edi", "r8d", "eax", 19),
                $round!(parity, "eax", "edx", "esi", "edi", "r8d", 20),
                $round!(parity, "r8d", "eax", "edx", "esi", "edi", 21),
                $round!(parity, "edi", "r8d", "eax", "edx", "esi", 22),
                $round!(parity, "esi", "edi", "r8d", "eax", "edx", 23),
                $round!(parity, "edx", "esi", "edi", "r8d", "eax", 24),
                $round!(parity, "eax", "edx", "esi", "edi", "r8d", 25),
                $round!(parity, "r8d", "eax", "edx", "esi", "edi", 26),
                $round!(parity, "edi", "r8d", "eax", "edx", "esi", 27),
                $round!(parity, "esi", "edi", "r8d", "eax", "edx", 28),
                $round!(parity, "edx", "esi", "edi", "r8d", "eax", 29),
                $round!(parity, "eax", "edx", "esi", "edi", "r8d", 30),
                $round!(parity, "r8d", "eax", "edx", "esi", "edi", 31),
                $round!(parity, "edi", "r8d", "eax", "edx", "esi", 32),
                $round!(parity, "esi", "edi", "r8d", "eax", "edx", 33),
                $round!(parity, "edx", "esi", "edi", "r8d", "eax", 34),
                $round!(parity, "eax", "edx", "esi", "edi", "r8d", 35),
                $round!(parity, "r8d", "eax", "edx", "esi", "edi", 36),
                $round!(parity, "edi", "r8d", "eax", "edx", "esi", 37),
                $round!(parity, "esi", "edi", "r8d", "eax", "edx", 38),
                $round!(parity, "edx", "esi", "edi", "r8d", "eax", 39),
                $round!(majority, "eax", "edx", "esi", "edi", "r8d", 40),
                $round!(majority, "r8d", "eax", "edx", "esi", "edi", 41),
                $round!(majority, "edi", "r8d", "eax", "edx", "esi", 42),
                $round!(majority, "esi", "edi", "r8d", "eax", "edx", 43),
                $round!(majority, "edx", "esi", "edi", "r8d", "eax", 44),
                $round!(majority, "eax", "edx", "esi", "edi", "r8d", 45),
                $round!(majority, "r8d", "eax", "edx", "esi", "edi", 46),
                $round!(majority, "edi", "r8d", "eax", "edx", "esi", 47),
                $round!(majority, "esi", "edi", "r8d", "eax", "edx", 48),
                $round!(majority, "edx", "esi", "edi", "r8d", "eax", 49),
                $round!(majority, "eax", "edx", "esi", "edi", "r8d", 50),
                $round!(majority, "r8d", "eax", "edx", "esi", "edi", 51),
                $round!(majority, "edi", "r8d", "eax", "edx", "esi", 52),
                $round!(majority, "esi", "edi", "r8d", "eax", "edx", 53),
                $round!(majority, "edx", "esi", "edi", "r8d", "eax", 54),
                $round!(majority, "eax", "edx", "esi", "edi", "r8d", 55),
                $round!(majority, "r8d", "eax", "edx", "esi", "edi", 56),
                $round!(majority, "edi", "r8d", "eax", "edx", "esi", 57),
                $round!(majority, "esi", "edi", "r8d", "eax", "edx", 58),
                $round!(majority, "edx", "esi", "edi", "r8d", "eax", 59),
                $round!(parity, "eax", "edx", "esi", "edi", "r8d", 60),
                $round!(parity, "r8d", "eax", "edx", "esi", "edi", 61),
                $round!(parity, "edi", "r8d", "eax", "edx", "esi", 62),
                $round!(parity, "esi", "edi", "r8d", "eax", "edx", 63),
                $round!(parity, "edx", "esi", "edi", "r8d", "eax", 64),
                $round!(parity, "eax", "edx", "esi", "edi", "r8d", 65),
                $round!(parity, "r8d", "eax", "edx", "esi", "edi", 66),
                $round!(parity, "edi", "r8d", "eax", "edx", "esi", 67),
                $round!(parity, "esi", "edi", "r8d", "eax", "edx", 68),
                $round!(parity, "edx", "esi", "edi", "r8d", "eax", 69),
                $round!(parity, "eax", "edx", "esi", "edi", "r8d", 70),
                $round!(parity, "r8d", "eax", "edx", "esi", "edi", 71),
                $round!(parity, "edi", "r8d", "eax", "edx", "esi", 72),
                $round!(parity, "esi", "edi", "r8d", "eax", "edx", 73),
                $round!(parity, "edx", "esi", "edi", "r8d", "eax", 74),
                $round!(parity, "eax", "edx", "esi", "edi", "r8d", 75),
                $round!(parity, "r8d", "eax", "edx", "esi", "edi", 76),
                $round!(parity, "edi", "r8d", "eax", "edx", "esi", 77),
                $round!(parity, "esi", "edi", "r8d", "eax", "edx", 78),
                $round!(parity, "edx", "esi", "edi", "r8d", "eax", 79),
                in("r11") wk,
                inout("eax") a,
                inout("edx") b,
                inout("esi") c,
                inout("edi") d,
                inout("r8d") e,
                out("r9d") _,
                out("r10d") _,
                options(nostack, readonly),
            );
            state[0] = state[0].wrapping_add(a);
            state[1] = state[1].wrapping_add(b);
            state[2] = state[2].wrapping_add(c);
            state[3] = state[3].wrapping_add(d);
            state[4] = state[4].wrapping_add(e);
        }
    }
}
    };
}

block_function!(
    /// for any x86-64 with SSSE3
    #[target_feature(enable = "ssse3")]
    compress,
    round_asm
);
block_function!(
    /// VEX-encoded schedule and flag-free rotations
    #[target_feature(enable = "avx2,bmi1,bmi2")]
    compress_bmi,
    round_bmi
);
