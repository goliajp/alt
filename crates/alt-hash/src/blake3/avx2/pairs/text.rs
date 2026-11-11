//! the schedule and the rounds of one or two pairs of nodes, as assembly text

/// Stores at `$to` the four message vectors of the first round of the blocks at
/// `$first` and `$second`: the words 0 2 4 6, 1 3 5 7, 14 8 10 12 and 15 9 11 13 of
/// each. The last two serve the diagonals and are turned the way row 0 is.
macro_rules! first_schedule {
    ($first:expr, $second:expr, $to:expr) => {
        concat!(
            "vmovdqu xmm10, xmmword ptr [",
            $first,
            "]\n",
            "vmovdqu xmm11, xmmword ptr [",
            $first,
            " + 16]\n",
            "vmovdqu xmm12, xmmword ptr [",
            $first,
            " + 32]\n",
            "vmovdqu xmm13, xmmword ptr [",
            $first,
            " + 48]\n",
            "vinserti128 ymm10, ymm10, xmmword ptr [",
            $second,
            "], 1\n",
            "vinserti128 ymm11, ymm11, xmmword ptr [",
            $second,
            " + 16], 1\n",
            "vinserti128 ymm12, ymm12, xmmword ptr [",
            $second,
            " + 32], 1\n",
            "vinserti128 ymm13, ymm13, xmmword ptr [",
            $second,
            " + 48], 1\n",
            "vshufps ymm14, ymm10, ymm11, 0x88\n",
            "vmovdqa ymmword ptr [",
            $to,
            "], ymm14\n",
            "vshufps ymm14, ymm10, ymm11, 0xdd\n",
            "vmovdqa ymmword ptr [",
            $to,
            " + 32], ymm14\n",
            "vshufps ymm14, ymm12, ymm13, 0x88\n",
            "vpshufd ymm14, ymm14, 0x93\n",
            "vmovdqa ymmword ptr [",
            $to,
            " + 64], ymm14\n",
            "vshufps ymm14, ymm12, ymm13, 0xdd\n",
            "vpshufd ymm14, ymm14, 0x93\n",
            "vmovdqa ymmword ptr [",
            $to,
            " + 96], ymm14\n",
        )
    };
}

/// The message vectors of the next round from those of this one, with p, q, r, s
/// for the four at `$from`: p1 q1 q3 p2, p3 r2 p0 s3, s0 q0 r3 s1 and r1 s2 q2 r0.
/// Where two words already sit in the right half of a vector a blend, which any
/// port executes, gathers them, and six shuffles are left.
macro_rules! next_schedule {
    ($from:expr, $to:expr) => {
        concat!(
            "vmovdqa ymm10, ymmword ptr [",
            $from,
            "]\n",
            "vmovdqa ymm11, ymmword ptr [",
            $from,
            " + 32]\n",
            "vmovdqa ymm12, ymmword ptr [",
            $from,
            " + 64]\n",
            "vmovdqa ymm13, ymmword ptr [",
            $from,
            " + 96]\n",
            "vshufps ymm14, ymm10, ymm11, 0xd9\n",
            "vpshufd ymm14, ymm14, 0x78\n",
            "vmovdqa ymmword ptr [",
            $to,
            "], ymm14\n",
            "vpblendd ymm14, ymm10, ymm12, 0x44\n",
            "vpblendd ymm15, ymm10, ymm13, 0x88\n",
            "vshufps ymm14, ymm14, ymm15, 0xcb\n",
            "vmovdqa ymmword ptr [",
            $to,
            " + 32], ymm14\n",
            "vpunpckldq ymm14, ymm13, ymm11\n",
            "vpblendd ymm15, ymm12, ymm13, 0x22\n",
            "vshufps ymm14, ymm14, ymm15, 0x74\n",
            "vmovdqa ymmword ptr [",
            $to,
            " + 64], ymm14\n",
            "vpblendd ymm14, ymm12, ymm13, 0x44\n",
            "vpblendd ymm15, ymm11, ymm12, 0x11\n",
            "vshufps ymm14, ymm14, ymm15, 0x29\n",
            "vmovdqa ymmword ptr [",
            $to,
            " + 96], ymm14\n",
        )
    };
}

/// Half of the mixing of all four columns of one pair: rows `$a $b $c $d`, the
/// temporary `$t`, the message vector at `$m`.
macro_rules! half {
    ($a:literal, $b:literal, $c:literal, $d:literal, $t:literal, $m:expr,
     $shuffle:expr, $right:literal, $left:literal) => {
        concat!(
            "vpaddd ",
            $a,
            ", ",
            $a,
            ", ymmword ptr [",
            $m,
            "]\n",
            "vpaddd ",
            $a,
            ", ",
            $a,
            ", ",
            $b,
            "\n",
            "vpxor ",
            $d,
            ", ",
            $d,
            ", ",
            $a,
            "\n",
            "vpshufb ",
            $d,
            ", ",
            $d,
            ", ",
            $shuffle,
            "\n",
            "vpaddd ",
            $c,
            ", ",
            $c,
            ", ",
            $d,
            "\n",
            "vpxor ",
            $b,
            ", ",
            $b,
            ", ",
            $c,
            "\n",
            "vpsrld ",
            $t,
            ", ",
            $b,
            ", ",
            $right,
            "\n",
            "vpslld ",
            $b,
            ", ",
            $b,
            ", ",
            $left,
            "\n",
            "vpor ",
            $b,
            ", ",
            $b,
            ", ",
            $t,
            "\n",
        )
    };
}

/// One round of pair A, whose message vectors are the four from rdi - 128 on. Rows
/// 0, 2 and 3 turn so that the diagonals become columns, and back; row 1 stays,
/// being the last a half step produces and the first the next one needs.
macro_rules! round_a {
    () => {
        concat!(
            half!(
                "ymm0",
                "ymm1",
                "ymm2",
                "ymm3",
                "ymm8",
                "rdi - 128",
                "ymmword ptr [{rot}]",
                "12",
                "20"
            ),
            half!(
                "ymm0",
                "ymm1",
                "ymm2",
                "ymm3",
                "ymm8",
                "rdi - 96",
                "ymmword ptr [{rot} + 32]",
                "7",
                "25"
            ),
            "vpshufd ymm0, ymm0, 0x93\n",
            "vpshufd ymm3, ymm3, 0x4e\n",
            "vpshufd ymm2, ymm2, 0x39\n",
            half!(
                "ymm0",
                "ymm1",
                "ymm2",
                "ymm3",
                "ymm8",
                "rdi - 64",
                "ymmword ptr [{rot}]",
                "12",
                "20"
            ),
            half!(
                "ymm0",
                "ymm1",
                "ymm2",
                "ymm3",
                "ymm8",
                "rdi - 32",
                "ymmword ptr [{rot} + 32]",
                "7",
                "25"
            ),
            "vpshufd ymm0, ymm0, 0x39\n",
            "vpshufd ymm3, ymm3, 0x4e\n",
            "vpshufd ymm2, ymm2, 0x93\n",
        )
    };
}

/// One round of both pairs, half step by half step; pair B's message vectors are
/// the four from rdi on.
macro_rules! round_both {
    () => {
        concat!(
            half!(
                "ymm0",
                "ymm1",
                "ymm2",
                "ymm3",
                "ymm8",
                "rdi - 128",
                "ymmword ptr [{rot}]",
                "12",
                "20"
            ),
            half!(
                "ymm4",
                "ymm5",
                "ymm6",
                "ymm7",
                "ymm9",
                "rdi",
                "ymmword ptr [{rot}]",
                "12",
                "20"
            ),
            half!(
                "ymm0",
                "ymm1",
                "ymm2",
                "ymm3",
                "ymm8",
                "rdi - 96",
                "ymmword ptr [{rot} + 32]",
                "7",
                "25"
            ),
            half!(
                "ymm4",
                "ymm5",
                "ymm6",
                "ymm7",
                "ymm9",
                "rdi + 32",
                "ymmword ptr [{rot} + 32]",
                "7",
                "25"
            ),
            "vpshufd ymm0, ymm0, 0x93\n",
            "vpshufd ymm4, ymm4, 0x93\n",
            "vpshufd ymm3, ymm3, 0x4e\n",
            "vpshufd ymm7, ymm7, 0x4e\n",
            "vpshufd ymm2, ymm2, 0x39\n",
            "vpshufd ymm6, ymm6, 0x39\n",
            half!(
                "ymm0",
                "ymm1",
                "ymm2",
                "ymm3",
                "ymm8",
                "rdi - 64",
                "ymmword ptr [{rot}]",
                "12",
                "20"
            ),
            half!(
                "ymm4",
                "ymm5",
                "ymm6",
                "ymm7",
                "ymm9",
                "rdi + 64",
                "ymmword ptr [{rot}]",
                "12",
                "20"
            ),
            half!(
                "ymm0",
                "ymm1",
                "ymm2",
                "ymm3",
                "ymm8",
                "rdi - 32",
                "ymmword ptr [{rot} + 32]",
                "7",
                "25"
            ),
            half!(
                "ymm4",
                "ymm5",
                "ymm6",
                "ymm7",
                "ymm9",
                "rdi + 96",
                "ymmword ptr [{rot} + 32]",
                "7",
                "25"
            ),
            "vpshufd ymm0, ymm0, 0x39\n",
            "vpshufd ymm4, ymm4, 0x39\n",
            "vpshufd ymm3, ymm3, 0x4e\n",
            "vpshufd ymm7, ymm7, 0x4e\n",
            "vpshufd ymm2, ymm2, 0x93\n",
            "vpshufd ymm6, ymm6, 0x93\n",
        )
    };
}
