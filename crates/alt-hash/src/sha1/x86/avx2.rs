//! SHA-1 compression for x86-64 with AVX2 and BMI, two blocks per pass.
//!
//! The message schedules of two blocks are computed together, one block in each half
//! of a 256-bit register, and written out with the round constants added. That work
//! rides on the rounds: the second half of a pair's schedule is computed during the
//! rounds of its first block, and the first half of the next pair's during the rounds
//! of its second block, so neither block's rounds wait for it.

use super::super::K;

/// the shuffle that turns a group of big-endian words around, then the round constants
#[repr(C, align(16))]
struct Table([u32; 8]);
static TABLE: Table = Table([
    0x0001_0203,
    0x0405_0607,
    0x0809_0a0b,
    0x0c0d_0e0f,
    K[0],
    K[1],
    K[2],
    K[3],
]);

/// schedule words plus round constant: twenty groups of four, first block in the low
/// half of each group and second block in the high half
#[repr(C, align(32))]
struct Words(std::mem::MaybeUninit<[u8; 640]>);

#[macro_use]
mod schedule;

/// Before the first round of a block: the working variables are in eax, ecx, edx, esi,
/// edi. The rounds keep b already rotated and the round function of the coming round
/// ready in a sixth register, here r8d.
macro_rules! start {
    () => {
        concat!(
            "mov r8d, ecx\n",
            "andn r9d, ecx, esi\n",
            "and r8d, edx\n",
            "xor r8d, r9d\n",
            "rorx ecx, ecx, 2\n",
        )
    };
}

/// One round: e += word + f + rol(a, 5), and the round function of the next round,
/// named by the first argument, is left in the register of `$a` while `$a` rotated
/// goes to the register `$f` gave up. The order of the instructions is the one that
/// measured fastest; what does not depend on `$a`, the value computed last, is taken
/// out of its path, even where that costs an instruction.
macro_rules! round {
    (choose, $a:literal, $b:literal, $c:literal, $d:literal, $e:literal, $f:literal, $word:expr) => {
        concat!(
            "add ",
            $e,
            ", ",
            $word,
            "\n",
            "andn r9d, ",
            $a,
            ", ",
            $c,
            "\n",
            "add ",
            $e,
            ", ",
            $f,
            "\n",
            "rorx r10d, ",
            $a,
            ", 27\n",
            "rorx ",
            $f,
            ", ",
            $a,
            ", 2\n",
            "and ",
            $a,
            ", ",
            $b,
            "\n",
            "add ",
            $e,
            ", r10d\n",
            "xor ",
            $a,
            ", r9d\n",
        )
    };
    (parity, $a:literal, $b:literal, $c:literal, $d:literal, $e:literal, $f:literal, $word:expr) => {
        concat!(
            "add ",
            $e,
            ", ",
            $word,
            "\n",
            "add ",
            $e,
            ", ",
            $f,
            "\n",
            "rorx r10d, ",
            $a,
            ", 27\n",
            "rorx ",
            $f,
            ", ",
            $a,
            ", 2\n",
            "mov r9d, ",
            $b,
            "\n",
            "xor r9d, ",
            $c,
            "\n",
            "xor ",
            $a,
            ", r9d\n",
            "add ",
            $e,
            ", r10d\n",
        )
    };
    // a & (b ^ c) and b & c share no bits; the second does not involve a and is added
    // straight to d, which is e in the next round
    (majority, $a:literal, $b:literal, $c:literal, $d:literal, $e:literal, $f:literal, $word:expr) => {
        concat!(
            "add ",
            $e,
            ", ",
            $word,
            "\n",
            "add ",
            $e,
            ", ",
            $f,
            "\n",
            "rorx r10d, ",
            $a,
            ", 27\n",
            "rorx ",
            $f,
            ", ",
            $a,
            ", 2\n",
            "mov r9d, ",
            $b,
            "\n",
            "xor r9d, ",
            $c,
            "\n",
            "and ",
            $a,
            ", r9d\n",
            "add ",
            $e,
            ", r10d\n",
            "mov r9d, ",
            $b,
            "\n",
            "and r9d, ",
            $c,
            "\n",
            "add ",
            $d,
            ", r9d\n",
        )
    };
    // the last round of a block prepares nothing
    (last, $a:literal, $b:literal, $c:literal, $d:literal, $e:literal, $f:literal, $word:expr) => {
        concat!(
            "add ",
            $e,
            ", ",
            $word,
            "\n",
            "add ",
            $e,
            ", ",
            $f,
            "\n",
            "rorx r10d, ",
            $a,
            ", 27\n",
            "add ",
            $e,
            ", r10d\n",
        )
    };
}

/// The four rounds of group `$g`, each named by the round function of the round after
/// it; `$lane` is 0 for the first block and 16 for the second. A round hands its six
/// registers on in another order, so the roles are spelled out per group.
macro_rules! four {
    ($n0:ident, $n1:ident, $n2:ident, $n3:ident; $g:literal, $lane:literal; $a:literal, $b:literal, $c:literal, $d:literal, $e:literal, $f:literal) => {
        concat!(
            round!(
                $n0,
                $a,
                $b,
                $c,
                $d,
                $e,
                $f,
                concat!("dword ptr [r11 + ", $g, "*32 + 0 + ", $lane, "]")
            ),
            round!(
                $n1,
                $e,
                $f,
                $b,
                $c,
                $d,
                $a,
                concat!("dword ptr [r11 + ", $g, "*32 + 4 + ", $lane, "]")
            ),
            round!(
                $n2,
                $d,
                $a,
                $f,
                $b,
                $c,
                $e,
                concat!("dword ptr [r11 + ", $g, "*32 + 8 + ", $lane, "]")
            ),
            round!(
                $n3,
                $c,
                $e,
                $a,
                $f,
                $b,
                $d,
                concat!("dword ptr [r11 + ", $g, "*32 + 12 + ", $lane, "]")
            ),
        )
    };
}

/// folds whole 64-byte blocks into `state`
#[target_feature(enable = "avx2,bmi1,bmi2")]
pub(in super::super) unsafe fn compress(state: &mut [u32; 5], blocks: &[u8]) {
    let (blocks, _) = blocks.as_chunks::<64>();
    let even = blocks.len() & !1;
    unsafe {
        if even != 0 {
            pairs(state, blocks.as_ptr().cast(), even);
        }
        if let Some(last) = blocks.get(even) {
            // a lone block goes through as both halves of a pair that stops after the first
            let mut twice = [0u8; 128];
            twice[..64].copy_from_slice(last);
            twice[64..].copy_from_slice(last);
            pairs(state, twice.as_ptr(), 1);
        }
    }
}

/// `count` blocks at `blocks`: an even number, or one block followed by 64 readable bytes
#[target_feature(enable = "avx2,bmi1,bmi2")]
unsafe fn pairs(state: &mut [u32; 5], blocks: *const u8, count: usize) {
    let mut words = Words(std::mem::MaybeUninit::uninit());
    // r14 is the pair being processed, r13 the next one (or the same again when there
    // is none, whose schedule is then computed for nothing), r15 the blocks left.
    unsafe {
        std::arch::asm!(
            "vbroadcasti128 ymm9, xmmword ptr [r13]\n",
            "vpbroadcastd ymm12, dword ptr [r13 + 16]\n",
            "vpbroadcastd ymm13, dword ptr [r13 + 20]\n",
            "vpbroadcastd ymm14, dword ptr [r13 + 24]\n",
            "vpbroadcastd ymm15, dword ptr [r13 + 28]\n",
            "mov eax, dword ptr [r12]\n",
            "mov ecx, dword ptr [r12 + 4]\n",
            "mov edx, dword ptr [r12 + 8]\n",
            "mov esi, dword ptr [r12 + 12]\n",
            "mov edi, dword ptr [r12 + 16]\n",
            load!(0, "ymm0", "xmm0", "r14"),
            load!(1, "ymm1", "xmm1", "r14"),
            load!(2, "ymm2", "xmm2", "r14"),
            load!(3, "ymm3", "xmm3", "r14"),
            early!(4, "ymm12", "ymm4", "ymm3", "ymm2", "ymm1", "ymm0"),
            early!(5, "ymm13", "ymm5", "ymm4", "ymm3", "ymm2", "ymm1"),
            early!(6, "ymm13", "ymm6", "ymm5", "ymm4", "ymm3", "ymm2"),
            early!(7, "ymm13", "ymm7", "ymm6", "ymm5", "ymm4", "ymm3"),
            late!(8, "ymm13", "ymm0", "ymm7", "ymm6", "ymm4", "ymm1"),
            late!(9, "ymm13", "ymm1", "ymm0", "ymm7", "ymm5", "ymm2"),
            "3:\n",
            start!(),
            late!(10, "ymm14", "ymm2", "ymm1", "ymm0", "ymm6", "ymm3"),
            four!(choose, choose, choose, choose; 0, 0; "eax", "ecx", "edx", "esi", "edi", "r8d"),
            late!(11, "ymm14", "ymm3", "ymm2", "ymm1", "ymm7", "ymm4"),
            four!(choose, choose, choose, choose; 1, 0; "ecx", "esi", "edi", "eax", "r8d", "edx"),
            late!(12, "ymm14", "ymm4", "ymm3", "ymm2", "ymm0", "ymm5"),
            four!(choose, choose, choose, choose; 2, 0; "esi", "eax", "r8d", "ecx", "edx", "edi"),
            late!(13, "ymm14", "ymm5", "ymm4", "ymm3", "ymm1", "ymm6"),
            four!(choose, choose, choose, choose; 3, 0; "eax", "ecx", "edx", "esi", "edi", "r8d"),
            late!(14, "ymm14", "ymm6", "ymm5", "ymm4", "ymm2", "ymm7"),
            four!(choose, choose, choose, parity; 4, 0; "ecx", "esi", "edi", "eax", "r8d", "edx"),
            late!(15, "ymm15", "ymm7", "ymm6", "ymm5", "ymm3", "ymm0"),
            four!(parity, parity, parity, parity; 5, 0; "esi", "eax", "r8d", "ecx", "edx", "edi"),
            late!(16, "ymm15", "ymm0", "ymm7", "ymm6", "ymm4", "ymm1"),
            four!(parity, parity, parity, parity; 6, 0; "eax", "ecx", "edx", "esi", "edi", "r8d"),
            late!(17, "ymm15", "ymm1", "ymm0", "ymm7", "ymm5", "ymm2"),
            four!(parity, parity, parity, parity; 7, 0; "ecx", "esi", "edi", "eax", "r8d", "edx"),
            late!(18, "ymm15", "ymm2", "ymm1", "ymm0", "ymm6", "ymm3"),
            four!(parity, parity, parity, parity; 8, 0; "esi", "eax", "r8d", "ecx", "edx", "edi"),
            late!(19, "ymm15", "ymm3", "ymm2", "ymm1", "ymm7", "ymm4"),
            four!(parity, parity, parity, majority; 9, 0; "eax", "ecx", "edx", "esi", "edi", "r8d"),
            four!(majority, majority, majority, majority; 10, 0; "ecx", "esi", "edi", "eax", "r8d", "edx"),
            four!(majority, majority, majority, majority; 11, 0; "esi", "eax", "r8d", "ecx", "edx", "edi"),
            four!(majority, majority, majority, majority; 12, 0; "eax", "ecx", "edx", "esi", "edi", "r8d"),
            four!(majority, majority, majority, majority; 13, 0; "ecx", "esi", "edi", "eax", "r8d", "edx"),
            four!(majority, majority, majority, parity; 14, 0; "esi", "eax", "r8d", "ecx", "edx", "edi"),
            four!(parity, parity, parity, parity; 15, 0; "eax", "ecx", "edx", "esi", "edi", "r8d"),
            four!(parity, parity, parity, parity; 16, 0; "ecx", "esi", "edi", "eax", "r8d", "edx"),
            four!(parity, parity, parity, parity; 17, 0; "esi", "eax", "r8d", "ecx", "edx", "edi"),
            four!(parity, parity, parity, parity; 18, 0; "eax", "ecx", "edx", "esi", "edi", "r8d"),
            four!(parity, parity, parity, last; 19, 0; "ecx", "esi", "edi", "eax", "r8d", "edx"),
            "add esi, dword ptr [r12 + 0]\n",
            "mov dword ptr [r12 + 0], esi\n",
            "add edi, dword ptr [r12 + 4]\n",
            "mov dword ptr [r12 + 4], edi\n",
            "add r8d, dword ptr [r12 + 8]\n",
            "mov dword ptr [r12 + 8], r8d\n",
            "add ecx, dword ptr [r12 + 12]\n",
            "mov dword ptr [r12 + 12], ecx\n",
            "add edx, dword ptr [r12 + 16]\n",
            "mov dword ptr [r12 + 16], edx\n",
            "mov eax, esi\n",
            "mov esi, ecx\n",
            "mov ecx, edi\n",
            "mov edi, edx\n",
            "mov edx, r8d\n",
            "sub r15, 1\n",
            "jz 4f\n",
            "lea r13, [r14 + 128]\n",
            "cmp r15, 3\n",
            "cmovb r13, r14\n",
            start!(),
            four!(choose, choose, choose, choose; 0, 16; "eax", "ecx", "edx", "esi", "edi", "r8d"),
            load!(0, "ymm0", "xmm0", "r13"),
            four!(choose, choose, choose, choose; 1, 16; "ecx", "esi", "edi", "eax", "r8d", "edx"),
            load!(1, "ymm1", "xmm1", "r13"),
            four!(choose, choose, choose, choose; 2, 16; "esi", "eax", "r8d", "ecx", "edx", "edi"),
            load!(2, "ymm2", "xmm2", "r13"),
            four!(choose, choose, choose, choose; 3, 16; "eax", "ecx", "edx", "esi", "edi", "r8d"),
            load!(3, "ymm3", "xmm3", "r13"),
            four!(choose, choose, choose, parity; 4, 16; "ecx", "esi", "edi", "eax", "r8d", "edx"),
            early!(4, "ymm12", "ymm4", "ymm3", "ymm2", "ymm1", "ymm0"),
            four!(parity, parity, parity, parity; 5, 16; "esi", "eax", "r8d", "ecx", "edx", "edi"),
            early!(5, "ymm13", "ymm5", "ymm4", "ymm3", "ymm2", "ymm1"),
            four!(parity, parity, parity, parity; 6, 16; "eax", "ecx", "edx", "esi", "edi", "r8d"),
            early!(6, "ymm13", "ymm6", "ymm5", "ymm4", "ymm3", "ymm2"),
            four!(parity, parity, parity, parity; 7, 16; "ecx", "esi", "edi", "eax", "r8d", "edx"),
            early!(7, "ymm13", "ymm7", "ymm6", "ymm5", "ymm4", "ymm3"),
            four!(parity, parity, parity, parity; 8, 16; "esi", "eax", "r8d", "ecx", "edx", "edi"),
            late!(8, "ymm13", "ymm0", "ymm7", "ymm6", "ymm4", "ymm1"),
            four!(parity, parity, parity, majority; 9, 16; "eax", "ecx", "edx", "esi", "edi", "r8d"),
            late!(9, "ymm13", "ymm1", "ymm0", "ymm7", "ymm5", "ymm2"),
            four!(majority, majority, majority, majority; 10, 16; "ecx", "esi", "edi", "eax", "r8d", "edx"),
            four!(majority, majority, majority, majority; 11, 16; "esi", "eax", "r8d", "ecx", "edx", "edi"),
            four!(majority, majority, majority, majority; 12, 16; "eax", "ecx", "edx", "esi", "edi", "r8d"),
            four!(majority, majority, majority, majority; 13, 16; "ecx", "esi", "edi", "eax", "r8d", "edx"),
            four!(majority, majority, majority, parity; 14, 16; "esi", "eax", "r8d", "ecx", "edx", "edi"),
            four!(parity, parity, parity, parity; 15, 16; "eax", "ecx", "edx", "esi", "edi", "r8d"),
            four!(parity, parity, parity, parity; 16, 16; "ecx", "esi", "edi", "eax", "r8d", "edx"),
            four!(parity, parity, parity, parity; 17, 16; "esi", "eax", "r8d", "ecx", "edx", "edi"),
            four!(parity, parity, parity, parity; 18, 16; "eax", "ecx", "edx", "esi", "edi", "r8d"),
            four!(parity, parity, parity, last; 19, 16; "ecx", "esi", "edi", "eax", "r8d", "edx"),
            "add esi, dword ptr [r12 + 0]\n",
            "mov dword ptr [r12 + 0], esi\n",
            "add edi, dword ptr [r12 + 4]\n",
            "mov dword ptr [r12 + 4], edi\n",
            "add r8d, dword ptr [r12 + 8]\n",
            "mov dword ptr [r12 + 8], r8d\n",
            "add ecx, dword ptr [r12 + 12]\n",
            "mov dword ptr [r12 + 12], ecx\n",
            "add edx, dword ptr [r12 + 16]\n",
            "mov dword ptr [r12 + 16], edx\n",
            "mov eax, esi\n",
            "mov esi, ecx\n",
            "mov ecx, edi\n",
            "mov edi, edx\n",
            "mov edx, r8d\n",
            "sub r15, 1\n",
            "jz 4f\n",
            "mov r14, r13\n",
            "jmp 3b\n",
            "4:\n",
            "vzeroupper\n",
            in("r11") words.0.as_mut_ptr(),
            in("r12") state.as_mut_ptr(),
            inout("r13") &raw const TABLE => _,
            inout("r14") blocks => _,
            inout("r15") count => _,
            out("eax") _,
            out("ecx") _,
            out("edx") _,
            out("esi") _,
            out("edi") _,
            out("r8d") _,
            out("r9d") _,
            out("r10d") _,
            out("ymm0") _,
            out("ymm1") _,
            out("ymm2") _,
            out("ymm3") _,
            out("ymm4") _,
            out("ymm5") _,
            out("ymm6") _,
            out("ymm7") _,
            out("ymm9") _,
            out("ymm10") _,
            out("ymm11") _,
            out("ymm12") _,
            out("ymm13") _,
            out("ymm14") _,
            out("ymm15") _,
            options(nostack),
        );
    }
}
