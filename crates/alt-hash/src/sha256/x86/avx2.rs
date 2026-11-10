//! SHA-256 compression for x86-64 with AVX2 and BMI, two blocks per pass.
//!
//! The message schedules of two blocks are computed together, one block in each half
//! of a 256-bit register, and written out with the round constants added. The rounds
//! then run over each block's half in turn. Both parts are short loops, so the whole
//! thing stays inside the decoded-instruction cache.

use super::super::K;

/// The round constants with every group of four repeated for the second block, then
/// three shuffles: big-endian words turned around, and the low words of two 64-bit
/// halves gathered into the lower or the upper pair of words.
#[repr(C, align(32))]
struct Table([u32; 140]);
static TABLE: Table = Table({
    let mut table = [0u32; 140];
    let mut at = 0;
    while at < 64 {
        table[at / 4 * 8 + at % 4] = K[at];
        table[at / 4 * 8 + 4 + at % 4] = K[at];
        at += 1;
    }
    let shuffles = [
        [0x0001_0203, 0x0405_0607, 0x0809_0a0b, 0x0c0d_0e0f],
        [0x0302_0100, 0x0b0a_0908, 0x8080_8080, 0x8080_8080],
        [0x8080_8080, 0x8080_8080, 0x0302_0100, 0x0b0a_0908],
    ];
    let mut at = 0;
    while at < 12 {
        table[128 + at] = shuffles[at / 4][at % 4];
        at += 1;
    }
    table
});

/// Schedule words plus round constant: sixteen groups of four, first block in the low
/// half of each group and second block in the high half. A copy of the round constants
/// follows, so that one cursor reaches both.
#[repr(C, align(32))]
struct Words(std::mem::MaybeUninit<[u8; 1024]>);

/// loads one group of four words of both blocks
macro_rules! load {
    ($g:literal, $y:literal, $x:literal) => {
        concat!(
            "vmovdqu ",
            $x,
            ", xmmword ptr [r14 + ",
            $g,
            "*16]\n",
            "vinserti128 ",
            $y,
            ", ",
            $y,
            ", xmmword ptr [r10 + ",
            $g,
            "*16], 1\n",
            "vpshufb ",
            $y,
            ", ",
            $y,
            ", ymm13\n",
            "vpaddd ymm8, ",
            $y,
            ", ymmword ptr [r11 + 512 + ",
            $g,
            "*32]\n",
            "vmovdqa ymmword ptr [r11 + ",
            $g,
            "*32], ymm8\n",
        )
    };
}

/// The next group of four schedule words, written 128 bytes ahead of the round cursor
/// in r15. `$x0` holds the group sixteen words back and
/// receives the new one; `$x1`..`$x3` are the groups after it. The small sigma of the
/// two newest words is needed by the last two words of the same group, so it is taken
/// in two halves; a rotation of a word is a shift of the word doubled into 64 bits.
macro_rules! extend {
    ($i:literal, $x0:literal, $x1:literal, $x2:literal, $x3:literal) => {
        concat!(
            "vpalignr ymm8, ",
            $x1,
            ", ",
            $x0,
            ", 4\n",
            "vpalignr ymm9, ",
            $x3,
            ", ",
            $x2,
            ", 4\n",
            "vpaddd ",
            $x0,
            ", ",
            $x0,
            ", ymm9\n",
            "vpsrld ymm9, ymm8, 7\n",
            "vpslld ymm10, ymm8, 25\n",
            "vpor ymm9, ymm9, ymm10\n",
            "vpsrld ymm10, ymm8, 18\n",
            "vpxor ymm9, ymm9, ymm10\n",
            "vpslld ymm10, ymm8, 14\n",
            "vpxor ymm9, ymm9, ymm10\n",
            "vpsrld ymm8, ymm8, 3\n",
            "vpxor ymm9, ymm9, ymm8\n",
            "vpaddd ",
            $x0,
            ", ",
            $x0,
            ", ymm9\n",
            "vpshufd ymm8, ",
            $x3,
            ", 0xfa\n",
            "vpsrlq ymm9, ymm8, 17\n",
            "vpsrlq ymm10, ymm8, 19\n",
            "vpsrld ymm8, ymm8, 10\n",
            "vpxor ymm9, ymm9, ymm10\n",
            "vpxor ymm8, ymm8, ymm9\n",
            "vpshufb ymm8, ymm8, ymm14\n",
            "vpaddd ",
            $x0,
            ", ",
            $x0,
            ", ymm8\n",
            "vpshufd ymm8, ",
            $x0,
            ", 0x50\n",
            "vpsrlq ymm9, ymm8, 17\n",
            "vpsrlq ymm10, ymm8, 19\n",
            "vpsrld ymm8, ymm8, 10\n",
            "vpxor ymm9, ymm9, ymm10\n",
            "vpxor ymm8, ymm8, ymm9\n",
            "vpshufb ymm8, ymm8, ymm15\n",
            "vpaddd ",
            $x0,
            ", ",
            $x0,
            ", ymm8\n",
            "vpaddd ymm8, ",
            $x0,
            ", ymmword ptr [r15 + 640 + ",
            $i,
            "*32]\n",
            "vmovdqa ymmword ptr [r15 + 128 + ",
            $i,
            "*32], ymm8\n",
        )
    };
}

/// One round. Only `$d` and `$h` change, so the eight roles rotate through the
/// registers. `$y` arrives holding b ^ c, which was a ^ b one round earlier, and `$z`
/// leaves holding a ^ b for the next round: the majority is b where a and b agree
/// and c where they differ. Sums of two registers are written as `lea`, which only
/// runs on two of the four integer units and so leaves the other two to the six
/// rotations of a round, which can run nowhere else.
macro_rules! round {
    ($a:literal, $b:literal, $c:literal, $d:literal, $e:literal, $f:literal, $g:literal, $h:literal, $y:literal, $z:literal, $at:literal) => {
        concat!(
            "add ",
            $h,
            ", dword ptr [r15 + ",
            $at,
            "]\n",
            "andn r11d, ",
            $e,
            ", ",
            $g,
            "\n",
            "lea ",
            $h,
            ", [",
            $h,
            " + r11d]\n",
            "mov r12d, ",
            $e,
            "\n",
            "and r12d, ",
            $f,
            "\n",
            "lea ",
            $h,
            ", [",
            $h,
            " + r12d]\n",
            "rorx r11d, ",
            $e,
            ", 25\n",
            "rorx r12d, ",
            $e,
            ", 11\n",
            "xor r11d, r12d\n",
            "rorx r12d, ",
            $e,
            ", 6\n",
            "xor r11d, r12d\n",
            "lea ",
            $h,
            ", [",
            $h,
            " + r11d]\n",
            "lea ",
            $d,
            ", [",
            $d,
            " + ",
            $h,
            "]\n",
            "mov ",
            $z,
            ", ",
            $a,
            "\n",
            "xor ",
            $z,
            ", ",
            $b,
            "\n",
            "and ",
            $y,
            ", ",
            $z,
            "\n",
            "xor ",
            $y,
            ", ",
            $b,
            "\n",
            "lea ",
            $h,
            ", [",
            $h,
            " + ",
            $y,
            "]\n",
            "rorx r11d, ",
            $a,
            ", 22\n",
            "rorx r12d, ",
            $a,
            ", 13\n",
            "xor r11d, r12d\n",
            "rorx r12d, ",
            $a,
            ", 2\n",
            "xor r11d, r12d\n",
            "lea ",
            $h,
            ", [",
            $h,
            " + r11d]\n",
        )
    };
}

/// folds whole 64-byte blocks into `state`
#[target_feature(enable = "avx2,bmi1,bmi2")]
pub(in super::super) unsafe fn compress(state: &mut [u32; 8], blocks: &[u8]) {
    let count = blocks.len() / 64;
    if count == 0 {
        return;
    }
    let mut words = Words(std::mem::MaybeUninit::uninit());
    // the table is 560 bytes and the buffer 1024: the constants fit behind the words
    unsafe {
        std::ptr::copy_nonoverlapping(
            (&raw const TABLE).cast::<u8>(),
            words.0.as_mut_ptr().cast::<u8>().add(512),
            512,
        );
    }
    // The rounds need every general register, so the pointers wait on the stack:
    // [rsp] where the plain rounds of the current block end, [rsp + 8] state,
    // [rsp + 16] input, [rsp + 24] blocks left, [rsp + 32] words, [rsp + 40] where the
    // rounds that carry the schedule end. The first block's rounds 0..48 compute the
    // schedule of both blocks one loop pass ahead of its use; everything after that
    // is the plain eight-round loop.
    unsafe {
        std::arch::asm!(
            "sub rsp, 48\n",
            "mov qword ptr [rsp + 8], r12\n",
            "mov qword ptr [rsp + 32], r11\n",
            "vbroadcasti128 ymm13, xmmword ptr [r13 + 512]\n",
            "vbroadcasti128 ymm14, xmmword ptr [r13 + 528]\n",
            "vbroadcasti128 ymm15, xmmword ptr [r13 + 544]\n",
            "2:\n",
            // a lone last block is paired with itself; its second pass never runs
            "lea r10, [r14 + 64]\n",
            "cmp r15, 1\n",
            "cmove r10, r14\n",
            load!(0, "ymm0", "xmm0"),
            load!(1, "ymm1", "xmm1"),
            load!(2, "ymm2", "xmm2"),
            load!(3, "ymm3", "xmm3"),
            "mov qword ptr [rsp + 16], r14\n",
            "mov qword ptr [rsp + 24], r15\n",
            "mov r15, r11\n",
            "lea rax, [r11 + 384]\n",
            "mov qword ptr [rsp + 40], rax\n",
            "lea rax, [r11 + 512]\n",
            "mov qword ptr [rsp], rax\n",
            "mov r12, qword ptr [rsp + 8]\n",
            "mov eax, dword ptr [r12 + 0]\n",
            "mov ecx, dword ptr [r12 + 4]\n",
            "mov edx, dword ptr [r12 + 8]\n",
            "mov esi, dword ptr [r12 + 12]\n",
            "mov edi, dword ptr [r12 + 16]\n",
            "mov r8d, dword ptr [r12 + 20]\n",
            "mov r9d, dword ptr [r12 + 24]\n",
            "mov r10d, dword ptr [r12 + 28]\n",
            "mov r13d, ecx\n",
            "xor r13d, edx\n",
            "8:\n",
            round!("eax", "ecx", "edx", "esi", "edi", "r8d", "r9d", "r10d", "r13d", "r14d", 0),
            round!("r10d", "eax", "ecx", "edx", "esi", "edi", "r8d", "r9d", "r14d", "r13d", 4),
            round!("r9d", "r10d", "eax", "ecx", "edx", "esi", "edi", "r8d", "r13d", "r14d", 8),
            round!("r8d", "r9d", "r10d", "eax", "ecx", "edx", "esi", "edi", "r14d", "r13d", 12),
            extend!(0, "ymm0", "ymm1", "ymm2", "ymm3"),
            round!("edi", "r8d", "r9d", "r10d", "eax", "ecx", "edx", "esi", "r13d", "r14d", 32),
            round!("esi", "edi", "r8d", "r9d", "r10d", "eax", "ecx", "edx", "r14d", "r13d", 36),
            round!("edx", "esi", "edi", "r8d", "r9d", "r10d", "eax", "ecx", "r13d", "r14d", 40),
            round!("ecx", "edx", "esi", "edi", "r8d", "r9d", "r10d", "eax", "r14d", "r13d", 44),
            extend!(1, "ymm1", "ymm2", "ymm3", "ymm0"),
            round!("eax", "ecx", "edx", "esi", "edi", "r8d", "r9d", "r10d", "r13d", "r14d", 64),
            round!("r10d", "eax", "ecx", "edx", "esi", "edi", "r8d", "r9d", "r14d", "r13d", 68),
            round!("r9d", "r10d", "eax", "ecx", "edx", "esi", "edi", "r8d", "r13d", "r14d", 72),
            round!("r8d", "r9d", "r10d", "eax", "ecx", "edx", "esi", "edi", "r14d", "r13d", 76),
            extend!(2, "ymm2", "ymm3", "ymm0", "ymm1"),
            round!("edi", "r8d", "r9d", "r10d", "eax", "ecx", "edx", "esi", "r13d", "r14d", 96),
            round!("esi", "edi", "r8d", "r9d", "r10d", "eax", "ecx", "edx", "r14d", "r13d", 100),
            round!("edx", "esi", "edi", "r8d", "r9d", "r10d", "eax", "ecx", "r13d", "r14d", 104),
            round!("ecx", "edx", "esi", "edi", "r8d", "r9d", "r10d", "eax", "r14d", "r13d", 108),
            extend!(3, "ymm3", "ymm0", "ymm1", "ymm2"),
            "add r15, 128\n",
            "cmp r15, qword ptr [rsp + 40]\n",
            "jne 8b\n",
            "7:\n",
            round!("eax", "ecx", "edx", "esi", "edi", "r8d", "r9d", "r10d", "r13d", "r14d", 0),
            round!("r10d", "eax", "ecx", "edx", "esi", "edi", "r8d", "r9d", "r14d", "r13d", 4),
            round!("r9d", "r10d", "eax", "ecx", "edx", "esi", "edi", "r8d", "r13d", "r14d", 8),
            round!("r8d", "r9d", "r10d", "eax", "ecx", "edx", "esi", "edi", "r14d", "r13d", 12),
            round!("edi", "r8d", "r9d", "r10d", "eax", "ecx", "edx", "esi", "r13d", "r14d", 32),
            round!("esi", "edi", "r8d", "r9d", "r10d", "eax", "ecx", "edx", "r14d", "r13d", 36),
            round!("edx", "esi", "edi", "r8d", "r9d", "r10d", "eax", "ecx", "r13d", "r14d", 40),
            round!("ecx", "edx", "esi", "edi", "r8d", "r9d", "r10d", "eax", "r14d", "r13d", 44),
            "add r15, 64\n",
            "cmp r15, qword ptr [rsp]\n",
            "jne 7b\n",
            "mov r12, qword ptr [rsp + 8]\n",
            "add eax, dword ptr [r12 + 0]\n",
            "mov dword ptr [r12 + 0], eax\n",
            "add ecx, dword ptr [r12 + 4]\n",
            "mov dword ptr [r12 + 4], ecx\n",
            "add edx, dword ptr [r12 + 8]\n",
            "mov dword ptr [r12 + 8], edx\n",
            "add esi, dword ptr [r12 + 12]\n",
            "mov dword ptr [r12 + 12], esi\n",
            "add edi, dword ptr [r12 + 16]\n",
            "mov dword ptr [r12 + 16], edi\n",
            "add r8d, dword ptr [r12 + 20]\n",
            "mov dword ptr [r12 + 20], r8d\n",
            "add r9d, dword ptr [r12 + 24]\n",
            "mov dword ptr [r12 + 24], r9d\n",
            "add r10d, dword ptr [r12 + 28]\n",
            "mov dword ptr [r12 + 28], r10d\n",
            "sub qword ptr [rsp + 24], 1\n",
            "jz 4f\n",
            "mov r11, qword ptr [rsp + 32]\n",
            "lea r12, [r11 + 512]\n",
            "cmp r15, r12\n",
            "jne 5f\n",
            "lea r15, [r11 + 16]\n",
            "lea r12, [r11 + 528]\n",
            "mov qword ptr [rsp], r12\n",
            "mov r12, qword ptr [rsp + 8]\n",
            "mov eax, dword ptr [r12 + 0]\n",
            "mov ecx, dword ptr [r12 + 4]\n",
            "mov edx, dword ptr [r12 + 8]\n",
            "mov esi, dword ptr [r12 + 12]\n",
            "mov edi, dword ptr [r12 + 16]\n",
            "mov r8d, dword ptr [r12 + 20]\n",
            "mov r9d, dword ptr [r12 + 24]\n",
            "mov r10d, dword ptr [r12 + 28]\n",
            "mov r13d, ecx\n",
            "xor r13d, edx\n",
            "jmp 7b\n",
            "5:\n",
            "mov r14, qword ptr [rsp + 16]\n",
            "mov r15, qword ptr [rsp + 24]\n",
            "add r14, 128\n",
            "jmp 2b\n",
            "4:\n",
            "add rsp, 48\n",
            "vzeroupper\n",
            inout("r11") words.0.as_mut_ptr() => _,
            inout("r12") state.as_mut_ptr() => _,
            inout("r13") &raw const TABLE => _,
            inout("r14") blocks.as_ptr() => _,
            inout("r15") count => _,
            out("eax") _,
            out("ecx") _,
            out("edx") _,
            out("esi") _,
            out("edi") _,
            out("r8d") _,
            out("r9d") _,
            out("r10") _,
            out("ymm0") _,
            out("ymm1") _,
            out("ymm2") _,
            out("ymm3") _,
            out("ymm8") _,
            out("ymm9") _,
            out("ymm10") _,
            out("ymm13") _,
            out("ymm14") _,
            out("ymm15") _,
        );
    }
}
