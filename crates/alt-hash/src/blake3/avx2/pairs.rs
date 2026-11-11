//! Four or two nodes at a time, each compressed as four rows of four words.
//!
//! A 256-bit register holds the same row of two nodes, one per half, so every
//! instruction of the single-block compression works on two nodes; with four nodes
//! two such pairs run interleaved. This takes fewer instructions per block than one
//! vector lane per node, which at this width would use only half of each register.
//!
//! The message vectors of each round are kept in memory, which leaves the registers
//! to the two states: pair A in ymm0-ymm3, pair B in ymm4-ymm7. Those of a round are
//! derived from those of the round before, ahead of the round that uses them, so
//! that this work fills the ports the mixing leaves idle.

use super::super::IV;
use super::super::lanes::Batch;
use std::arch::asm;
use std::arch::x86_64::*;
use std::mem::MaybeUninit;

#[macro_use]
mod text;

/// byte shuffles that rotate each 32-bit word right by 16 and by 8
#[repr(C, align(32))]
struct Rotations([u8; 64]);
static ROTATIONS: Rotations = Rotations([
    2, 3, 0, 1, 6, 7, 4, 5, 10, 11, 8, 9, 14, 15, 12, 13, 2, 3, 0, 1, 6, 7, 4, 5, 10, 11, 8, 9, 14,
    15, 12, 13, 1, 2, 3, 0, 5, 6, 7, 4, 9, 10, 11, 8, 13, 14, 15, 12, 1, 2, 3, 0, 5, 6, 7, 4, 9,
    10, 11, 8, 13, 14, 15, 12,
]);

/// the message vectors of seven rounds: four for pair A, then four for pair B
#[repr(C, align(32))]
struct Schedule([u8; 7 * 256]);

/// the chaining values of two nodes, words 0-3 and words 4-7, one node per half
type Value = [__m256i; 2];

#[inline(always)]
unsafe fn initial(key: &[u32; 8]) -> Value {
    // reads the eight words of `key`
    unsafe {
        [
            _mm256_broadcastsi128_si256(_mm_loadu_si128(key.as_ptr().cast())),
            _mm256_broadcastsi128_si256(_mm_loadu_si128(key.as_ptr().add(4).cast())),
        ]
    }
}

/// state words 12-15 of the nodes that count `counter` and, if `increment`, one more
#[inline(always)]
unsafe fn last_row(counter: u64, increment: bool, flags: u32) -> __m256i {
    let second = counter + increment as u64;
    unsafe {
        _mm256_set_epi32(
            flags as i32,
            64,
            (second >> 32) as i32,
            second as i32,
            flags as i32,
            64,
            (counter >> 32) as i32,
            counter as i32,
        )
    }
}

/// writes the chaining values of both nodes, 64 bytes at `out`
#[inline(always)]
unsafe fn store(value: Value, out: *mut u8) {
    unsafe {
        let first = _mm256_permute2x128_si256::<0x20>(value[0], value[1]);
        let second = _mm256_permute2x128_si256::<0x31>(value[0], value[1]);
        _mm256_storeu_si256(out.cast(), first);
        _mm256_storeu_si256(out.add(32).cast(), second);
    }
}

/// Compresses the four nodes in `input`, the first of which counts `counter`, and
/// writes their chaining values to `out`.
#[target_feature(enable = "avx2")]
pub(super) unsafe fn compress_four(batch: &Batch, input: &[u8], counter: u64, out: &mut [u8]) {
    assert!(input.len() == 4 * batch.node_len && out.len() == 4 * 32);
    let blocks = batch.node_len / 64;
    let step = 2 * batch.increment as u64;
    let mut schedule = MaybeUninit::<Schedule>::uninit();
    // every load stays inside `input` and the stores inside `out`, whose lengths were
    // just checked; the assembly reads four blocks, and of `schedule` only what it wrote before
    unsafe {
        let iv = _mm256_broadcastsi128_si256(_mm_loadu_si128(IV.as_ptr().cast()));
        let ([mut a0, mut a1], [mut b0, mut b1]) = (initial(batch.key), initial(batch.key));
        let mut flags = batch.flags | batch.flags_start;
        for block in 0..blocks {
            if block + 1 == blocks {
                flags |= batch.flags_end;
            }
            let at = input.as_ptr().add(block * 64);
            asm!(
                first_schedule!("{n0}", "{n0} + {len}", "rdi - 128"),
                first_schedule!("{n2}", "{n2} + {len}", "rdi"),
                next_schedule!("rdi - 128", "rax - 128"),
                next_schedule!("rdi", "rax"),
                round_both!(),
                "add rdi, 256\n",
                "add rax, 256\n",
                next_schedule!("rdi - 128", "rax - 128"),
                next_schedule!("rdi", "rax"),
                round_both!(),
                "add rdi, 256\n",
                "add rax, 256\n",
                next_schedule!("rdi - 128", "rax - 128"),
                next_schedule!("rdi", "rax"),
                round_both!(),
                "add rdi, 256\n",
                "add rax, 256\n",
                next_schedule!("rdi - 128", "rax - 128"),
                next_schedule!("rdi", "rax"),
                round_both!(),
                "add rdi, 256\n",
                "add rax, 256\n",
                next_schedule!("rdi - 128", "rax - 128"),
                next_schedule!("rdi", "rax"),
                round_both!(),
                "add rdi, 256\n",
                "add rax, 256\n",
                next_schedule!("rdi - 128", "rax - 128"),
                next_schedule!("rdi", "rax"),
                round_both!(),
                "add rdi, 256\n",
                "add rax, 256\n",
                round_both!(),
                "vpxor ymm0, ymm0, ymm2\n",
                "vpxor ymm1, ymm1, ymm3\n",
                "vpxor ymm4, ymm4, ymm6\n",
                "vpxor ymm5, ymm5, ymm7\n",
                n0 = in(reg) at,
                n2 = in(reg) at.add(2 * batch.node_len),
                len = in(reg) batch.node_len,
                inout("rdi") schedule.as_mut_ptr().byte_add(128) => _,
                inout("rax") schedule.as_mut_ptr().byte_add(384) => _,
                rot = in(reg) &raw const ROTATIONS,
                inout("ymm0") a0,
                inout("ymm1") a1,
                inout("ymm2") iv => _,
                inout("ymm3") last_row(counter, batch.increment, flags) => _,
                inout("ymm4") b0,
                inout("ymm5") b1,
                inout("ymm6") iv => _,
                inout("ymm7") last_row(counter + step, batch.increment, flags) => _,
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
            flags = batch.flags;
        }
        store([a0, a1], out.as_mut_ptr());
        store([b0, b1], out.as_mut_ptr().add(64));
    }
}

/// Compresses the two nodes in `input`, the first of which counts `counter`, and
/// writes their chaining values to `out`.
#[target_feature(enable = "avx2")]
pub(super) unsafe fn compress_two(batch: &Batch, input: &[u8], counter: u64, out: &mut [u8]) {
    assert!(input.len() == 2 * batch.node_len && out.len() == 2 * 32);
    let blocks = batch.node_len / 64;
    let mut schedule = MaybeUninit::<Schedule>::uninit();
    // as in `compress_four`, with two blocks
    unsafe {
        let iv = _mm256_broadcastsi128_si256(_mm_loadu_si128(IV.as_ptr().cast()));
        let [mut a0, mut a1] = initial(batch.key);
        let mut flags = batch.flags | batch.flags_start;
        for block in 0..blocks {
            if block + 1 == blocks {
                flags |= batch.flags_end;
            }
            asm!(
                first_schedule!("{n0}", "{n0} + {len}", "rdi - 128"),
                next_schedule!("rdi - 128", "rax - 128"),
                round_a!(),
                "add rdi, 256\n",
                "add rax, 256\n",
                next_schedule!("rdi - 128", "rax - 128"),
                round_a!(),
                "add rdi, 256\n",
                "add rax, 256\n",
                next_schedule!("rdi - 128", "rax - 128"),
                round_a!(),
                "add rdi, 256\n",
                "add rax, 256\n",
                next_schedule!("rdi - 128", "rax - 128"),
                round_a!(),
                "add rdi, 256\n",
                "add rax, 256\n",
                next_schedule!("rdi - 128", "rax - 128"),
                round_a!(),
                "add rdi, 256\n",
                "add rax, 256\n",
                next_schedule!("rdi - 128", "rax - 128"),
                round_a!(),
                "add rdi, 256\n",
                "add rax, 256\n",
                round_a!(),
                "vpxor ymm0, ymm0, ymm2\n",
                "vpxor ymm1, ymm1, ymm3\n",
                n0 = in(reg) input.as_ptr().add(block * 64),
                len = in(reg) batch.node_len,
                inout("rdi") schedule.as_mut_ptr().byte_add(128) => _,
                inout("rax") schedule.as_mut_ptr().byte_add(384) => _,
                rot = in(reg) &raw const ROTATIONS,
                inout("ymm0") a0,
                inout("ymm1") a1,
                inout("ymm2") iv => _,
                inout("ymm3") last_row(counter, batch.increment, flags) => _,
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
            flags = batch.flags;
        }
        store([a0, a1], out.as_mut_ptr());
    }
}
