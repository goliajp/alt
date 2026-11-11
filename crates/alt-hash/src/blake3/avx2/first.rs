//! the first round on eight lanes, as assembly text

/// The first half step of the first round, which also sets up state words 8-15:
/// the xor that begins words 12-15 and the addition that begins words 8-11 take
/// their starting values straight from memory.
macro_rules! first_half {
    () => {
        concat!(
            "vpaddd ymm0, ymm0, ymmword ptr [",
            slot!(0),
            "]\n",
            "vpaddd ymm1, ymm1, ymmword ptr [",
            slot!(2),
            "]\n",
            "vpaddd ymm2, ymm2, ymmword ptr [",
            slot!(4),
            "]\n",
            "vpaddd ymm3, ymm3, ymmword ptr [",
            slot!(6),
            "]\n",
            "vpaddd ymm0, ymm0, ymm4\n",
            "vpaddd ymm1, ymm1, ymm5\n",
            "vpaddd ymm2, ymm2, ymm6\n",
            "vpaddd ymm3, ymm3, ymm7\n",
            "vpxor ymm12, ymm0, ymmword ptr [rdx + 32]\n",
            "vpxor ymm13, ymm1, ymmword ptr [rdx + 64]\n",
            "vpxor ymm14, ymm2, ymmword ptr [rdx + 96]\n",
            "vpxor ymm15, ymm3, ymmword ptr [rdx + rsi + 320]\n",
            "vbroadcasti128 ymm8, xmmword ptr [rcx]\n",
            "vpshufb ymm12, ymm12, ymm8\n",
            "vpshufb ymm13, ymm13, ymm8\n",
            "vpshufb ymm14, ymm14, ymm8\n",
            "vpshufb ymm15, ymm15, ymm8\n",
            "vpaddd ymm8, ymm12, ymmword ptr [rdx - 128]\n",
            "vpaddd ymm9, ymm13, ymmword ptr [rdx - 96]\n",
            "vpaddd ymm10, ymm14, ymmword ptr [rdx - 64]\n",
            "vpaddd ymm11, ymm15, ymmword ptr [rdx - 32]\n",
            "vpxor ymm4, ymm4, ymm8\n",
            "vpxor ymm5, ymm5, ymm9\n",
            "vpxor ymm6, ymm6, ymm10\n",
            "vpxor ymm7, ymm7, ymm11\n",
            "vmovdqa ymmword ptr [rdx], ymm8\n",
            "vpsrld ymm8, ymm4, 12\n",
            "vpslld ymm4, ymm4, 20\n",
            "vpor ymm4, ymm4, ymm8\n",
            "vpsrld ymm8, ymm5, 12\n",
            "vpslld ymm5, ymm5, 20\n",
            "vpor ymm5, ymm5, ymm8\n",
            "vpsrld ymm8, ymm6, 12\n",
            "vpslld ymm6, ymm6, 20\n",
            "vpor ymm6, ymm6, ymm8\n",
            "vpsrld ymm8, ymm7, 12\n",
            "vpslld ymm7, ymm7, 20\n",
            "vpor ymm7, ymm7, ymm8\n",
        )
    };
}

/// the first round, on the message words in their own order
macro_rules! first_round {
    () => {
        concat!(
            first_half!(),
            half!(
                "ymm8",
                "ymmword",
                "32",
                "vbroadcasti128 ymm8, xmmword ptr",
                ("ymm0", "ymm4", "ymm12"),
                ("ymm1", "ymm5", "ymm9", "ymm13"),
                ("ymm2", "ymm6", "ymm10", "ymm14"),
                ("ymm3", "ymm7", "ymm11", "ymm15"),
                1,
                3,
                5,
                7,
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
                12,
                8,
                10,
                14,
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
                13,
                9,
                11,
                15,
                16,
                7,
                25
            ),
        )
    };
}
