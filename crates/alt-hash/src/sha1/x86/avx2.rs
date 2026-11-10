//! SHA-1 compression for x86-64 with AVX2 and BMI, two blocks per pass.
//!
//! The message schedules of two blocks are computed together, one block in each half
//! of a 256-bit register, and written out with the round constants added. The eighty
//! rounds exist once and run over each block's half in turn, which keeps the whole
//! loop inside the decoded-instruction cache.

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
            ", ymm9\n",
            "vpaddd ymm10, ",
            $y,
            ", ymm12\n",
            "vmovdqa ymmword ptr [r11 + ",
            $g,
            "*32], ymm10\n",
        )
    };
}

/// Schedule group `$g` for words 16..32 into `$w`, from the groups one to four back.
/// The last word of a group depends on its first, which is patched in at the end.
macro_rules! early {
    ($g:literal, $k:literal, $w:literal, $p1:literal, $p2:literal, $p3:literal, $p4:literal) => {
        concat!(
            "vpalignr ",
            $w,
            ", ",
            $p3,
            ", ",
            $p4,
            ", 8\n",
            "vpsrldq ymm10, ",
            $p1,
            ", 4\n",
            "vpxor ",
            $w,
            ", ",
            $w,
            ", ",
            $p4,
            "\n",
            "vpxor ymm10, ymm10, ",
            $p2,
            "\n",
            "vpxor ",
            $w,
            ", ",
            $w,
            ", ymm10\n",
            "vpslldq ymm11, ",
            $w,
            ", 12\n",
            "vpsrld ymm10, ",
            $w,
            ", 31\n",
            "vpaddd ",
            $w,
            ", ",
            $w,
            ", ",
            $w,
            "\n",
            "vpor ",
            $w,
            ", ",
            $w,
            ", ymm10\n",
            "vpsrld ymm10, ymm11, 30\n",
            "vpslld ymm11, ymm11, 2\n",
            "vpxor ",
            $w,
            ", ",
            $w,
            ", ymm10\n",
            "vpxor ",
            $w,
            ", ",
            $w,
            ", ymm11\n",
            "vpaddd ymm10, ",
            $w,
            ", ",
            $k,
            "\n",
            "vmovdqa ymmword ptr [r11 + ",
            $g,
            "*32], ymm10\n",
        )
    };
}

/// Schedule group `$g` for words 32..80. `$w` holds the group eight back and receives
/// the new one: w[t] = rol(w[t-6] ^ w[t-16] ^ w[t-28] ^ w[t-32], 2), which has no
/// dependency inside the group.
macro_rules! late {
    ($g:literal, $k:literal, $w:literal, $p1:literal, $p2:literal, $p4:literal, $p7:literal) => {
        concat!(
            "vpalignr ymm10, ",
            $p1,
            ", ",
            $p2,
            ", 8\n",
            "vpxor ",
            $w,
            ", ",
            $w,
            ", ",
            $p7,
            "\n",
            "vpxor ymm10, ymm10, ",
            $p4,
            "\n",
            "vpxor ",
            $w,
            ", ",
            $w,
            ", ymm10\n",
            "vpsrld ymm10, ",
            $w,
            ", 30\n",
            "vpslld ",
            $w,
            ", ",
            $w,
            ", 2\n",
            "vpor ",
            $w,
            ", ",
            $w,
            ", ymm10\n",
            "vpaddd ymm10, ",
            $w,
            ", ",
            $k,
            "\n",
            "vmovdqa ymmword ptr [r11 + ",
            $g,
            "*32], ymm10\n",
        )
    };
}

/// One round. `$b` rotated goes to the spare register `$n`, which frees `$b` itself to
/// be consumed by the round function without a copy. The rotation of `$a`, the value
/// computed last, joins the sum at the end.
macro_rules! round {
    (choose, $a:literal, $b:literal, $c:literal, $d:literal, $e:literal, $n:literal, $word:expr) => {
        concat!(
            "andn r9d, ",
            $b,
            ", ",
            $d,
            "\n",
            "rorx ",
            $n,
            ", ",
            $b,
            ", 2\n",
            "and ",
            $b,
            ", ",
            $c,
            "\n",
            "add ",
            $e,
            ", ",
            $word,
            "\n",
            "add ",
            $e,
            ", r9d\n",
            "add ",
            $e,
            ", ",
            $b,
            "\n",
            "rorx r9d, ",
            $a,
            ", 27\n",
            "add ",
            $e,
            ", r9d\n",
        )
    };
    (parity, $a:literal, $b:literal, $c:literal, $d:literal, $e:literal, $n:literal, $word:expr) => {
        concat!(
            "rorx ",
            $n,
            ", ",
            $b,
            ", 2\n",
            "xor ",
            $b,
            ", ",
            $c,
            "\n",
            "xor ",
            $b,
            ", ",
            $d,
            "\n",
            "add ",
            $e,
            ", ",
            $word,
            "\n",
            "add ",
            $e,
            ", ",
            $b,
            "\n",
            "rorx r9d, ",
            $a,
            ", 27\n",
            "add ",
            $e,
            ", r9d\n",
        )
    };
    // the majority is d where b and c differ and c where they agree; the two terms
    // share no bits, so both are simply added
    (majority, $a:literal, $b:literal, $c:literal, $d:literal, $e:literal, $n:literal, $word:expr) => {
        concat!(
            "rorx ",
            $n,
            ", ",
            $b,
            ", 2\n",
            "xor ",
            $b,
            ", ",
            $c,
            "\n",
            "andn r9d, ",
            $b,
            ", ",
            $c,
            "\n",
            "and ",
            $b,
            ", ",
            $d,
            "\n",
            "add ",
            $e,
            ", ",
            $word,
            "\n",
            "add ",
            $e,
            ", r9d\n",
            "add ",
            $e,
            ", ",
            $b,
            "\n",
            "rorx r9d, ",
            $a,
            ", 27\n",
            "add ",
            $e,
            ", r9d\n",
        )
    };
}

/// The four rounds of a group. `r13` points 128 bytes past the start of an eight-group
/// stretch of one block's words, so every offset fits a signed byte.
macro_rules! four {
    ($f:ident, $g:literal, $a:literal, $b:literal, $c:literal, $d:literal, $e:literal, $n:literal) => {
        concat!(
            round!(
                $f,
                $a,
                $b,
                $c,
                $d,
                $e,
                $n,
                concat!("dword ptr [r13 + ", $g, "*32 - 128]")
            ),
            round!(
                $f,
                $e,
                $a,
                $n,
                $c,
                $d,
                $b,
                concat!("dword ptr [r13 + ", $g, "*32 - 124]")
            ),
            round!(
                $f,
                $d,
                $e,
                $b,
                $n,
                $c,
                $a,
                concat!("dword ptr [r13 + ", $g, "*32 - 120]")
            ),
            round!(
                $f,
                $c,
                $d,
                $a,
                $b,
                $n,
                $e,
                concat!("dword ptr [r13 + ", $g, "*32 - 116]")
            ),
        )
    };
}

/// folds whole 64-byte blocks into `state`
#[target_feature(enable = "avx2,bmi1,bmi2")]
pub(in super::super) unsafe fn compress(state: &mut [u32; 5], blocks: &[u8]) {
    let count = blocks.len() / 64;
    if count == 0 {
        return;
    }
    let mut words = Words(std::mem::MaybeUninit::uninit());
    // A round consumes and produces registers that rotate through six names, so the
    // working variables end a block in other registers than they started in and are
    // moved back. r15 counts the blocks left, r10 is zero during the first block of a
    // pair, r13 walks that block's words.
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
            "2:\n",
            // a lone last block is paired with itself; its second pass never runs
            "lea r10, [r14 + 64]\n",
            "cmp r15, 1\n",
            "cmove r10, r14\n",
            load!(0, "ymm0", "xmm0"),
            load!(1, "ymm1", "xmm1"),
            load!(2, "ymm2", "xmm2"),
            load!(3, "ymm3", "xmm3"),
            early!(4, "ymm12", "ymm4", "ymm3", "ymm2", "ymm1", "ymm0"),
            early!(5, "ymm13", "ymm5", "ymm4", "ymm3", "ymm2", "ymm1"),
            early!(6, "ymm13", "ymm6", "ymm5", "ymm4", "ymm3", "ymm2"),
            early!(7, "ymm13", "ymm7", "ymm6", "ymm5", "ymm4", "ymm3"),
            late!(8, "ymm13", "ymm0", "ymm7", "ymm6", "ymm4", "ymm1"),
            late!(9, "ymm13", "ymm1", "ymm0", "ymm7", "ymm5", "ymm2"),
            late!(10, "ymm14", "ymm2", "ymm1", "ymm0", "ymm6", "ymm3"),
            late!(11, "ymm14", "ymm3", "ymm2", "ymm1", "ymm7", "ymm4"),
            late!(12, "ymm14", "ymm4", "ymm3", "ymm2", "ymm0", "ymm5"),
            late!(13, "ymm14", "ymm5", "ymm4", "ymm3", "ymm1", "ymm6"),
            late!(14, "ymm14", "ymm6", "ymm5", "ymm4", "ymm2", "ymm7"),
            late!(15, "ymm15", "ymm7", "ymm6", "ymm5", "ymm3", "ymm0"),
            late!(16, "ymm15", "ymm0", "ymm7", "ymm6", "ymm4", "ymm1"),
            late!(17, "ymm15", "ymm1", "ymm0", "ymm7", "ymm5", "ymm2"),
            late!(18, "ymm15", "ymm2", "ymm1", "ymm0", "ymm6", "ymm3"),
            late!(19, "ymm15", "ymm3", "ymm2", "ymm1", "ymm7", "ymm4"),
            "xor r10d, r10d\n",
            "lea r13, [r11 + 128]\n",
            "3:\n",
            four!(choose, 0, "eax", "ecx", "edx", "esi", "edi", "r8d"),
            four!(choose, 1, "r8d", "edx", "edi", "eax", "ecx", "esi"),
            four!(choose, 2, "esi", "edi", "ecx", "r8d", "edx", "eax"),
            four!(choose, 3, "eax", "ecx", "edx", "esi", "edi", "r8d"),
            four!(choose, 4, "r8d", "edx", "edi", "eax", "ecx", "esi"),
            four!(parity, 5, "esi", "edi", "ecx", "r8d", "edx", "eax"),
            four!(parity, 6, "eax", "ecx", "edx", "esi", "edi", "r8d"),
            four!(parity, 7, "r8d", "edx", "edi", "eax", "ecx", "esi"),
            "lea r13, [r13 + 256]\n",
            four!(parity, 0, "esi", "edi", "ecx", "r8d", "edx", "eax"),
            four!(parity, 1, "eax", "ecx", "edx", "esi", "edi", "r8d"),
            four!(majority, 2, "r8d", "edx", "edi", "eax", "ecx", "esi"),
            four!(majority, 3, "esi", "edi", "ecx", "r8d", "edx", "eax"),
            four!(majority, 4, "eax", "ecx", "edx", "esi", "edi", "r8d"),
            four!(majority, 5, "r8d", "edx", "edi", "eax", "ecx", "esi"),
            four!(majority, 6, "esi", "edi", "ecx", "r8d", "edx", "eax"),
            four!(parity, 7, "eax", "ecx", "edx", "esi", "edi", "r8d"),
            "lea r13, [r13 + 256]\n",
            four!(parity, 0, "r8d", "edx", "edi", "eax", "ecx", "esi"),
            four!(parity, 1, "esi", "edi", "ecx", "r8d", "edx", "eax"),
            four!(parity, 2, "eax", "ecx", "edx", "esi", "edi", "r8d"),
            four!(parity, 3, "r8d", "edx", "edi", "eax", "ecx", "esi"),
            "add esi, dword ptr [r12 + 0]\n",
            "mov dword ptr [r12 + 0], esi\n",
            "add edi, dword ptr [r12 + 4]\n",
            "mov dword ptr [r12 + 4], edi\n",
            "add ecx, dword ptr [r12 + 8]\n",
            "mov dword ptr [r12 + 8], ecx\n",
            "add r8d, dword ptr [r12 + 12]\n",
            "mov dword ptr [r12 + 12], r8d\n",
            "add edx, dword ptr [r12 + 16]\n",
            "mov dword ptr [r12 + 16], edx\n",
            "mov eax, esi\n",
            "mov esi, r8d\n",
            "mov r9d, ecx\n",
            "mov ecx, edi\n",
            "mov edi, edx\n",
            "mov edx, r9d\n",
            "dec r15\n",
            "jz 4f\n",
            "test r10d, r10d\n",
            "jnz 5f\n",
            "mov r10d, 1\n",
            "lea r13, [r11 + 144]\n",
            "jmp 3b\n",
            "5:\n",
            "add r14, 128\n",
            "jmp 2b\n",
            "4:\n",
            "vzeroupper\n",
            in("r11") words.0.as_mut_ptr(),
            in("r12") state.as_mut_ptr(),
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
            out("ymm4") _,
            out("ymm5") _,
            out("ymm6") _,
            out("ymm7") _,
            out("ymm8") _,
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
