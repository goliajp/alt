//! the rounds on eight lanes, as assembly text

/// Where message vector `n` is: rdi and rax point 128 bytes into each half of the
/// sixteen, so that every one is within a one-byte displacement. The loop is too long
/// for the cache of decoded instructions, so its speed is its size in bytes.
macro_rules! slot {
    (0) => {
        "rdi - 128"
    };
    (1) => {
        "rdi - 96"
    };
    (2) => {
        "rdi - 64"
    };
    (3) => {
        "rdi - 32"
    };
    (4) => {
        "rdi"
    };
    (5) => {
        "rdi + 32"
    };
    (6) => {
        "rdi + 64"
    };
    (7) => {
        "rdi + 96"
    };
    (8) => {
        "rax - 128"
    };
    (9) => {
        "rax - 96"
    };
    (10) => {
        "rax - 64"
    };
    (11) => {
        "rax - 32"
    };
    (12) => {
        "rax"
    };
    (13) => {
        "rax + 32"
    };
    (14) => {
        "rax + 64"
    };
    (15) => {
        "rax + 96"
    };
}

/// Half of the mixing function on four independent groups of words (a, b, c, d) at
/// once: a += b + m; d = (d ^ a) rotated by a byte shuffle; c += d; b = (b ^ c)
/// rotated right by `$right`. The first group's c is the state word kept in memory at
/// [rdx]; the register `$t` holds the shuffle, then that c, then the shifted halves.
/// `$word` and `$stride` are the size of a vector, and `$mask` loads the shuffle.
macro_rules! half {
    ($t:literal, $word:literal, $stride:literal, $mask:literal,
     ($a0:literal, $b0:literal, $d0:literal), ($a1:literal, $b1:literal, $c1:literal, $d1:literal),
     ($a2:literal, $b2:literal, $c2:literal, $d2:literal), ($a3:literal, $b3:literal, $c3:literal, $d3:literal),
     $m0:tt, $m1:tt, $m2:tt, $m3:tt, $shuffle:literal, $right:literal, $left:literal) => {
        concat!(
            "vpaddd ",
            $a0,
            ", ",
            $a0,
            ", ",
            $word,
            " ptr [",
            slot!($m0),
            "]\n",
            "vpaddd ",
            $a1,
            ", ",
            $a1,
            ", ",
            $word,
            " ptr [",
            slot!($m1),
            "]\n",
            "vpaddd ",
            $a2,
            ", ",
            $a2,
            ", ",
            $word,
            " ptr [",
            slot!($m2),
            "]\n",
            "vpaddd ",
            $a3,
            ", ",
            $a3,
            ", ",
            $word,
            " ptr [",
            slot!($m3),
            "]\n",
            "vpaddd ",
            $a0,
            ", ",
            $a0,
            ", ",
            $b0,
            "\n",
            "vpaddd ",
            $a1,
            ", ",
            $a1,
            ", ",
            $b1,
            "\n",
            "vpaddd ",
            $a2,
            ", ",
            $a2,
            ", ",
            $b2,
            "\n",
            "vpaddd ",
            $a3,
            ", ",
            $a3,
            ", ",
            $b3,
            "\n",
            "vpxor ",
            $d0,
            ", ",
            $d0,
            ", ",
            $a0,
            "\n",
            "vpxor ",
            $d1,
            ", ",
            $d1,
            ", ",
            $a1,
            "\n",
            "vpxor ",
            $d2,
            ", ",
            $d2,
            ", ",
            $a2,
            "\n",
            "vpxor ",
            $d3,
            ", ",
            $d3,
            ", ",
            $a3,
            "\n",
            $mask,
            " [rcx + ",
            $shuffle,
            "]\n",
            "vpshufb ",
            $d0,
            ", ",
            $d0,
            ", ",
            $t,
            "\n",
            "vpshufb ",
            $d1,
            ", ",
            $d1,
            ", ",
            $t,
            "\n",
            "vpshufb ",
            $d2,
            ", ",
            $d2,
            ", ",
            $t,
            "\n",
            "vpshufb ",
            $d3,
            ", ",
            $d3,
            ", ",
            $t,
            "\n",
            "vpaddd ",
            $t,
            ", ",
            $d0,
            ", ",
            $word,
            " ptr [rdx]\n",
            "vpaddd ",
            $c1,
            ", ",
            $c1,
            ", ",
            $d1,
            "\n",
            "vpaddd ",
            $c2,
            ", ",
            $c2,
            ", ",
            $d2,
            "\n",
            "vpaddd ",
            $c3,
            ", ",
            $c3,
            ", ",
            $d3,
            "\n",
            "vpxor ",
            $b0,
            ", ",
            $b0,
            ", ",
            $t,
            "\n",
            "vpxor ",
            $b1,
            ", ",
            $b1,
            ", ",
            $c1,
            "\n",
            "vpxor ",
            $b2,
            ", ",
            $b2,
            ", ",
            $c2,
            "\n",
            "vpxor ",
            $b3,
            ", ",
            $b3,
            ", ",
            $c3,
            "\n",
            "vmovdqa ",
            $word,
            " ptr [rdx], ",
            $t,
            "\n",
            "vpsrld ",
            $t,
            ", ",
            $b0,
            ", ",
            $right,
            "\n",
            "vpslld ",
            $b0,
            ", ",
            $b0,
            ", ",
            $left,
            "\n",
            "vpor ",
            $b0,
            ", ",
            $b0,
            ", ",
            $t,
            "\n",
            "vpsrld ",
            $t,
            ", ",
            $b1,
            ", ",
            $right,
            "\n",
            "vpslld ",
            $b1,
            ", ",
            $b1,
            ", ",
            $left,
            "\n",
            "vpor ",
            $b1,
            ", ",
            $b1,
            ", ",
            $t,
            "\n",
            "vpsrld ",
            $t,
            ", ",
            $b2,
            ", ",
            $right,
            "\n",
            "vpslld ",
            $b2,
            ", ",
            $b2,
            ", ",
            $left,
            "\n",
            "vpor ",
            $b2,
            ", ",
            $b2,
            ", ",
            $t,
            "\n",
            "vpsrld ",
            $t,
            ", ",
            $b3,
            ", ",
            $right,
            "\n",
            "vpslld ",
            $b3,
            ", ",
            $b3,
            ", ",
            $left,
            "\n",
            "vpor ",
            $b3,
            ", ",
            $b3,
            ", ",
            $t,
            "\n",
        )
    };
}

/// One round on the message words in the given order: the four columns, then the four
/// diagonals, each in two halves. State word n is in ymm n, except word 8 in memory.
macro_rules! round {
    ($s0:tt, $s1:tt, $s2:tt, $s3:tt, $s4:tt, $s5:tt, $s6:tt, $s7:tt,
     $s8:tt, $s9:tt, $s10:tt, $s11:tt, $s12:tt, $s13:tt, $s14:tt, $s15:tt) => {
        concat!(
            half!(
                "ymm8",
                "ymmword",
                "32",
                "vbroadcasti128 ymm8, xmmword ptr",
                ("ymm0", "ymm4", "ymm12"),
                ("ymm1", "ymm5", "ymm9", "ymm13"),
                ("ymm2", "ymm6", "ymm10", "ymm14"),
                ("ymm3", "ymm7", "ymm11", "ymm15"),
                $s0,
                $s2,
                $s4,
                $s6,
                0,
                12,
                20
            ),
            half!(
                "ymm8",
                "ymmword",
                "32",
                "vbroadcasti128 ymm8, xmmword ptr",
                ("ymm0", "ymm4", "ymm12"),
                ("ymm1", "ymm5", "ymm9", "ymm13"),
                ("ymm2", "ymm6", "ymm10", "ymm14"),
                ("ymm3", "ymm7", "ymm11", "ymm15"),
                $s1,
                $s3,
                $s5,
                $s7,
                16,
                7,
                25
            ),
            half!(
                "ymm8",
                "ymmword",
                "32",
                "vbroadcasti128 ymm8, xmmword ptr",
                ("ymm2", "ymm7", "ymm13"),
                ("ymm0", "ymm5", "ymm10", "ymm15"),
                ("ymm1", "ymm6", "ymm11", "ymm12"),
                ("ymm3", "ymm4", "ymm9", "ymm14"),
                $s12,
                $s8,
                $s10,
                $s14,
                0,
                12,
                20
            ),
            half!(
                "ymm8",
                "ymmword",
                "32",
                "vbroadcasti128 ymm8, xmmword ptr",
                ("ymm2", "ymm7", "ymm13"),
                ("ymm0", "ymm5", "ymm10", "ymm15"),
                ("ymm1", "ymm6", "ymm11", "ymm12"),
                ("ymm3", "ymm4", "ymm9", "ymm14"),
                $s13,
                $s9,
                $s11,
                $s15,
                16,
                7,
                25
            ),
        )
    };
}
