//! The single-block compression as assembly, for processors with AVX2.
//!
//! One block is a chain of dependent steps, so its speed is the length of that chain.
//! Compiled from intrinsics the chain comes out longer: the rotation by 16 bits
//! becomes two shuffles, and the row that arrives last is added first.
//!
//! Registers: xmm0-xmm3 the rows of the state, xmm4-xmm7 the message words in the
//! order the round uses them, xmm8-xmm12 temporaries, xmm13 and xmm14 the byte
//! shuffles that rotate by 16 and by 8.

use std::arch::asm;
use std::arch::x86_64::*;

/// Half of the mixing of all four columns: the message words are in xmm9.
macro_rules! half {
    ($shuffle:literal, $right:literal, $left:literal) => {
        concat!(
            "vpaddd xmm0, xmm0, xmm9\n",
            "vpaddd xmm0, xmm0, xmm1\n",
            "vpxor xmm3, xmm3, xmm0\n",
            "vpshufb xmm3, xmm3, ",
            $shuffle,
            "\n",
            "vpaddd xmm2, xmm2, xmm3\n",
            "vpxor xmm1, xmm1, xmm2\n",
            "vpsrld xmm8, xmm1, ",
            $right,
            "\n",
            "vpslld xmm1, xmm1, ",
            $left,
            "\n",
            "vpor xmm1, xmm1, xmm8\n",
        )
    };
}

/// One round without the permutation of the message that follows it. Rows 0, 2 and
/// 3 turn so that the diagonals become columns, and back; row 1 stays, being the
/// last a half step produces and the first the next one needs.
macro_rules! mix {
    () => {
        concat!(
            "vshufps xmm9, xmm4, xmm5, 0x88\n",
            half!("xmm13", "12", "20"),
            "vshufps xmm9, xmm4, xmm5, 0xdd\n",
            half!("xmm14", "7", "25"),
            "vpshufd xmm0, xmm0, 0x93\n",
            "vpshufd xmm3, xmm3, 0x4e\n",
            "vpshufd xmm2, xmm2, 0x39\n",
            "vshufps xmm9, xmm6, xmm7, 0x88\n",
            "vpshufd xmm9, xmm9, 0x93\n",
            half!("xmm13", "12", "20"),
            "vshufps xmm9, xmm6, xmm7, 0xdd\n",
            "vpshufd xmm9, xmm9, 0x93\n",
            half!("xmm14", "7", "25"),
            "vpshufd xmm0, xmm0, 0x39\n",
            "vpshufd xmm3, xmm3, 0x4e\n",
            "vpshufd xmm2, xmm2, 0x93\n",
        )
    };
}

/// The message words in the order of the next round: with a, b, c, d for xmm4-xmm7,
/// a' = a2 b2 a3 c2, b' = b3 a0 b0 d1, c' = a1 c3 d0 b1, d' = c1 d2 d3 c0.
macro_rules! permute {
    () => {
        concat!(
            "vpunpckhdq xmm10, xmm4, xmm5\n",
            "vpshufd xmm9, xmm6, 0x80\n",
            "vpblendd xmm10, xmm10, xmm9, 0x8\n",
            "vpshufd xmm11, xmm5, 0x03\n",
            "vshufps xmm9, xmm4, xmm7, 0x50\n",
            "vpblendd xmm11, xmm11, xmm9, 0xa\n",
            "vshufps xmm12, xmm4, xmm5, 0x55\n",
            "vshufps xmm9, xmm6, xmm7, 0x0f\n",
            "vpblendd xmm12, xmm12, xmm9, 0x6\n",
            "vshufps xmm7, xmm6, xmm7, 0xe1\n",
            "vpshufd xmm7, xmm7, 0x78\n",
            "vmovdqa xmm4, xmm10\n",
            "vmovdqa xmm5, xmm11\n",
            "vmovdqa xmm6, xmm12\n",
        )
    };
}

/// The chaining value that follows `value` after `block`; `rows` are the constants
/// and the counter, length and flags that fill the lower half of the state.
///
/// The caller has detected AVX2.
#[inline(always)]
pub(super) unsafe fn compress_rows(
    value: [__m128i; 2],
    block: &[u8; 64],
    rows: [__m128i; 2],
    rotations: [__m128i; 2],
) -> [__m128i; 2] {
    let [mut first, mut second] = value;
    // reads the 64 bytes of `block` and changes only the registers it names
    unsafe {
        asm!(
            "vmovdqu xmm4, xmmword ptr [{block}]\n",
            "vmovdqu xmm5, xmmword ptr [{block} + 16]\n",
            "vmovdqu xmm6, xmmword ptr [{block} + 32]\n",
            "vmovdqu xmm7, xmmword ptr [{block} + 48]\n",
            mix!(),
            permute!(),
            mix!(),
            permute!(),
            mix!(),
            permute!(),
            mix!(),
            permute!(),
            mix!(),
            permute!(),
            mix!(),
            permute!(),
            mix!(),
            "vpxor xmm0, xmm0, xmm2\n",
            "vpxor xmm1, xmm1, xmm3\n",
            block = in(reg) block.as_ptr(),
            inout("xmm0") first,
            inout("xmm1") second,
            inout("xmm2") rows[0] => _,
            inout("xmm3") rows[1] => _,
            in("xmm13") rotations[0],
            in("xmm14") rotations[1],
            out("xmm4") _,
            out("xmm5") _,
            out("xmm6") _,
            out("xmm7") _,
            out("xmm8") _,
            out("xmm9") _,
            out("xmm10") _,
            out("xmm11") _,
            out("xmm12") _,
            options(pure, readonly, nostack),
        );
    }
    [first, second]
}
