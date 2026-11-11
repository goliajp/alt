//! Rounds of the four-stream Huffman loops in assembly, for processors
//! with BMI2: the compiler runs out of registers for the eight running
//! values and spills some of them every round.

/// One round of the single-symbol loop, five symbols from each of four
/// streams, with every value held in a register throughout.
///
/// # Safety
/// As for one round of the four-stream loop that calls it; the processor
/// must have BMI2.
#[inline(always)]
pub(crate) unsafe fn round_bmi2(
    bits: &mut [u64; 4],
    op: &[*mut u8; 4],
    table: *const u16,
    shift: u64,
) {
    macro_rules! symbol {
        ($bits:literal, $op:literal, $at:literal) => {
            concat!(
                "shrx {t}, {",
                $bits,
                "}, {shift}\n",
                "movzx {t:e}, word ptr [{table} + 2*{t}]\n",
                "mov byte ptr [{",
                $op,
                "} + ",
                $at,
                "], {t:l}\n",
                "shr {t:e}, 8\n",
                "shlx {",
                $bits,
                "}, {",
                $bits,
                "}, {t}\n",
            )
        };
    }
    macro_rules! each_stream {
        ($at:literal) => {
            concat!(
                symbol!("b0", "o0", $at),
                symbol!("b1", "o1", $at),
                symbol!("b2", "o2", $at),
                symbol!("b3", "o3", $at),
            )
        };
    }
    // safety: forwarded from the caller; only the output bytes are written
    unsafe {
        std::arch::asm!(
            each_stream!("0"),
            each_stream!("1"),
            each_stream!("2"),
            each_stream!("3"),
            each_stream!("4"),
            b0 = inout(reg) bits[0],
            b1 = inout(reg) bits[1],
            b2 = inout(reg) bits[2],
            b3 = inout(reg) bits[3],
            o0 = in(reg) op[0],
            o1 = in(reg) op[1],
            o2 = in(reg) op[2],
            o3 = in(reg) op[3],
            table = in(reg) table,
            shift = in(reg) shift,
            t = out(reg) _,
            options(nostack),
        );
    }
}

/// One round of the two-symbol loop, five lookups from each of four
/// streams, with every value held in a register throughout.
///
/// # Safety
/// As for one round of the four-stream loop that calls it; the processor
/// must have BMI2.
#[inline(always)]
pub(crate) unsafe fn pair_round_bmi2(
    bits: &mut [u64; 4],
    op: &mut [*mut u8; 4],
    table: *const u32,
    shift: u64,
) {
    // an entry holds two symbols, then the bits they take, then how many
    // they are; a shift count only looks at its low six bits
    macro_rules! lookup {
        ($bits:literal, $op:literal) => {
            concat!(
                "shrx {t}, {",
                $bits,
                "}, {shift}\n",
                "mov {t:e}, dword ptr [{table} + 4*{t}]\n",
                "mov word ptr [{",
                $op,
                "}], {t:x}\n",
                "shr {t:e}, 16\n",
                "shlx {",
                $bits,
                "}, {",
                $bits,
                "}, {t}\n",
                "shr {t:e}, 8\n",
                "add {",
                $op,
                "}, {t}\n",
            )
        };
    }
    macro_rules! each_stream {
        () => {
            concat!(
                lookup!("b0", "o0"),
                lookup!("b1", "o1"),
                lookup!("b2", "o2"),
                lookup!("b3", "o3"),
            )
        };
    }
    // safety: forwarded from the caller; only the output bytes are written
    unsafe {
        std::arch::asm!(
            each_stream!(),
            each_stream!(),
            each_stream!(),
            each_stream!(),
            each_stream!(),
            b0 = inout(reg) bits[0],
            b1 = inout(reg) bits[1],
            b2 = inout(reg) bits[2],
            b3 = inout(reg) bits[3],
            o0 = inout(reg) op[0],
            o1 = inout(reg) op[1],
            o2 = inout(reg) op[2],
            o3 = inout(reg) op[3],
            table = in(reg) table,
            shift = in(reg) shift,
            t = out(reg) _,
            options(nostack),
        );
    }
}
