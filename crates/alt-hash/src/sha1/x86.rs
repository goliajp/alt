//! SHA-1 compression for x86-64: SSSE3 here, AVX2 and the SHA instructions in submodules.

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

/// The block function for any x86-64 with SSSE3. The message schedule is computed four
/// words at a time with the round constant already added, so the eighty rounds only
/// read finished words.
#[target_feature(enable = "ssse3")]
pub(super) unsafe fn compress(state: &mut [u32; 5], blocks: &[u8]) {
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
                round_asm!(choose, "eax", "edx", "esi", "edi", "r8d", 0),
                round_asm!(choose, "r8d", "eax", "edx", "esi", "edi", 1),
                round_asm!(choose, "edi", "r8d", "eax", "edx", "esi", 2),
                round_asm!(choose, "esi", "edi", "r8d", "eax", "edx", 3),
                round_asm!(choose, "edx", "esi", "edi", "r8d", "eax", 4),
                round_asm!(choose, "eax", "edx", "esi", "edi", "r8d", 5),
                round_asm!(choose, "r8d", "eax", "edx", "esi", "edi", 6),
                round_asm!(choose, "edi", "r8d", "eax", "edx", "esi", 7),
                round_asm!(choose, "esi", "edi", "r8d", "eax", "edx", 8),
                round_asm!(choose, "edx", "esi", "edi", "r8d", "eax", 9),
                round_asm!(choose, "eax", "edx", "esi", "edi", "r8d", 10),
                round_asm!(choose, "r8d", "eax", "edx", "esi", "edi", 11),
                round_asm!(choose, "edi", "r8d", "eax", "edx", "esi", 12),
                round_asm!(choose, "esi", "edi", "r8d", "eax", "edx", 13),
                round_asm!(choose, "edx", "esi", "edi", "r8d", "eax", 14),
                round_asm!(choose, "eax", "edx", "esi", "edi", "r8d", 15),
                round_asm!(choose, "r8d", "eax", "edx", "esi", "edi", 16),
                round_asm!(choose, "edi", "r8d", "eax", "edx", "esi", 17),
                round_asm!(choose, "esi", "edi", "r8d", "eax", "edx", 18),
                round_asm!(choose, "edx", "esi", "edi", "r8d", "eax", 19),
                round_asm!(parity, "eax", "edx", "esi", "edi", "r8d", 20),
                round_asm!(parity, "r8d", "eax", "edx", "esi", "edi", 21),
                round_asm!(parity, "edi", "r8d", "eax", "edx", "esi", 22),
                round_asm!(parity, "esi", "edi", "r8d", "eax", "edx", 23),
                round_asm!(parity, "edx", "esi", "edi", "r8d", "eax", 24),
                round_asm!(parity, "eax", "edx", "esi", "edi", "r8d", 25),
                round_asm!(parity, "r8d", "eax", "edx", "esi", "edi", 26),
                round_asm!(parity, "edi", "r8d", "eax", "edx", "esi", 27),
                round_asm!(parity, "esi", "edi", "r8d", "eax", "edx", 28),
                round_asm!(parity, "edx", "esi", "edi", "r8d", "eax", 29),
                round_asm!(parity, "eax", "edx", "esi", "edi", "r8d", 30),
                round_asm!(parity, "r8d", "eax", "edx", "esi", "edi", 31),
                round_asm!(parity, "edi", "r8d", "eax", "edx", "esi", 32),
                round_asm!(parity, "esi", "edi", "r8d", "eax", "edx", 33),
                round_asm!(parity, "edx", "esi", "edi", "r8d", "eax", 34),
                round_asm!(parity, "eax", "edx", "esi", "edi", "r8d", 35),
                round_asm!(parity, "r8d", "eax", "edx", "esi", "edi", 36),
                round_asm!(parity, "edi", "r8d", "eax", "edx", "esi", 37),
                round_asm!(parity, "esi", "edi", "r8d", "eax", "edx", 38),
                round_asm!(parity, "edx", "esi", "edi", "r8d", "eax", 39),
                round_asm!(majority, "eax", "edx", "esi", "edi", "r8d", 40),
                round_asm!(majority, "r8d", "eax", "edx", "esi", "edi", 41),
                round_asm!(majority, "edi", "r8d", "eax", "edx", "esi", 42),
                round_asm!(majority, "esi", "edi", "r8d", "eax", "edx", 43),
                round_asm!(majority, "edx", "esi", "edi", "r8d", "eax", 44),
                round_asm!(majority, "eax", "edx", "esi", "edi", "r8d", 45),
                round_asm!(majority, "r8d", "eax", "edx", "esi", "edi", 46),
                round_asm!(majority, "edi", "r8d", "eax", "edx", "esi", 47),
                round_asm!(majority, "esi", "edi", "r8d", "eax", "edx", 48),
                round_asm!(majority, "edx", "esi", "edi", "r8d", "eax", 49),
                round_asm!(majority, "eax", "edx", "esi", "edi", "r8d", 50),
                round_asm!(majority, "r8d", "eax", "edx", "esi", "edi", 51),
                round_asm!(majority, "edi", "r8d", "eax", "edx", "esi", 52),
                round_asm!(majority, "esi", "edi", "r8d", "eax", "edx", 53),
                round_asm!(majority, "edx", "esi", "edi", "r8d", "eax", 54),
                round_asm!(majority, "eax", "edx", "esi", "edi", "r8d", 55),
                round_asm!(majority, "r8d", "eax", "edx", "esi", "edi", 56),
                round_asm!(majority, "edi", "r8d", "eax", "edx", "esi", 57),
                round_asm!(majority, "esi", "edi", "r8d", "eax", "edx", 58),
                round_asm!(majority, "edx", "esi", "edi", "r8d", "eax", 59),
                round_asm!(parity, "eax", "edx", "esi", "edi", "r8d", 60),
                round_asm!(parity, "r8d", "eax", "edx", "esi", "edi", 61),
                round_asm!(parity, "edi", "r8d", "eax", "edx", "esi", 62),
                round_asm!(parity, "esi", "edi", "r8d", "eax", "edx", 63),
                round_asm!(parity, "edx", "esi", "edi", "r8d", "eax", 64),
                round_asm!(parity, "eax", "edx", "esi", "edi", "r8d", 65),
                round_asm!(parity, "r8d", "eax", "edx", "esi", "edi", 66),
                round_asm!(parity, "edi", "r8d", "eax", "edx", "esi", 67),
                round_asm!(parity, "esi", "edi", "r8d", "eax", "edx", 68),
                round_asm!(parity, "edx", "esi", "edi", "r8d", "eax", 69),
                round_asm!(parity, "eax", "edx", "esi", "edi", "r8d", 70),
                round_asm!(parity, "r8d", "eax", "edx", "esi", "edi", 71),
                round_asm!(parity, "edi", "r8d", "eax", "edx", "esi", 72),
                round_asm!(parity, "esi", "edi", "r8d", "eax", "edx", 73),
                round_asm!(parity, "edx", "esi", "edi", "r8d", "eax", 74),
                round_asm!(parity, "eax", "edx", "esi", "edi", "r8d", 75),
                round_asm!(parity, "r8d", "eax", "edx", "esi", "edi", 76),
                round_asm!(parity, "edi", "r8d", "eax", "edx", "esi", 77),
                round_asm!(parity, "esi", "edi", "r8d", "eax", "edx", 78),
                round_asm!(parity, "edx", "esi", "edi", "r8d", "eax", 79),
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

pub(super) mod avx2;
pub(super) mod sha;
