//! Eight lanes in 256-bit vectors.
//!
//! The loop over the blocks of eight nodes is assembly: sixteen state vectors and a
//! temporary do not fit in the sixteen registers, and the compiler's choice of what
//! to keep in memory costs a sixth of the speed. Here fifteen state vectors stay in
//! registers, the sixteenth lives in memory with its register serving as the
//! temporary, and message words are read straight from memory.

use super::IV;
use super::lanes::Batch;
use std::arch::x86_64::*;
use std::mem::MaybeUninit;

mod pairs;
#[macro_use]
mod rounds;
#[macro_use]
mod first;

// The `unsafe` blocks below only wrap vector instructions. Those on registers are
// sound on any CPU with the features of the function they are inlined into; the
// loads and stores access exactly the bytes their callers are documented to own.

/// Rows of an 8x8 matrix of words to its columns.
#[inline(always)]
unsafe fn transpose(rows: [__m256i; 8]) -> [__m256i; 8] {
    unsafe {
        // the unpack instructions work inside each 128-bit half, so after two rounds
        // of them every vector holds two columns' halves: columns n and n + 4
        let ab_0145 = _mm256_unpacklo_epi32(rows[0], rows[1]);
        let ab_2367 = _mm256_unpackhi_epi32(rows[0], rows[1]);
        let cd_0145 = _mm256_unpacklo_epi32(rows[2], rows[3]);
        let cd_2367 = _mm256_unpackhi_epi32(rows[2], rows[3]);
        let ef_0145 = _mm256_unpacklo_epi32(rows[4], rows[5]);
        let ef_2367 = _mm256_unpackhi_epi32(rows[4], rows[5]);
        let gh_0145 = _mm256_unpacklo_epi32(rows[6], rows[7]);
        let gh_2367 = _mm256_unpackhi_epi32(rows[6], rows[7]);
        let abcd_04 = _mm256_unpacklo_epi64(ab_0145, cd_0145);
        let abcd_15 = _mm256_unpackhi_epi64(ab_0145, cd_0145);
        let abcd_26 = _mm256_unpacklo_epi64(ab_2367, cd_2367);
        let abcd_37 = _mm256_unpackhi_epi64(ab_2367, cd_2367);
        let efgh_04 = _mm256_unpacklo_epi64(ef_0145, gh_0145);
        let efgh_15 = _mm256_unpackhi_epi64(ef_0145, gh_0145);
        let efgh_26 = _mm256_unpacklo_epi64(ef_2367, gh_2367);
        let efgh_37 = _mm256_unpackhi_epi64(ef_2367, gh_2367);
        [
            _mm256_permute2x128_si256::<0x20>(abcd_04, efgh_04),
            _mm256_permute2x128_si256::<0x20>(abcd_15, efgh_15),
            _mm256_permute2x128_si256::<0x20>(abcd_26, efgh_26),
            _mm256_permute2x128_si256::<0x20>(abcd_37, efgh_37),
            _mm256_permute2x128_si256::<0x31>(abcd_04, efgh_04),
            _mm256_permute2x128_si256::<0x31>(abcd_15, efgh_15),
            _mm256_permute2x128_si256::<0x31>(abcd_26, efgh_26),
            _mm256_permute2x128_si256::<0x31>(abcd_37, efgh_37),
        ]
    }
}

#[inline(always)]
unsafe fn load(at: *const u8) -> __m256i {
    unsafe { _mm256_loadu_si256(at.cast()) }
}

/// byte shuffles that rotate each 32-bit word right by 16 and by 8
#[repr(C, align(16))]
struct Rotations([u8; 32]);
static ROTATIONS: Rotations = Rotations([
    2, 3, 0, 1, 6, 7, 4, 5, 10, 11, 8, 9, 14, 15, 12, 13, 1, 2, 3, 0, 5, 6, 7, 4, 9, 10, 11, 8, 13,
    14, 15, 12,
]);

#[repr(C, align(32))]
struct Vectors<const N: usize>([__m256i; N]);

/// Words `$o / 4` to `$o / 4 + 3` of the block at rsi in each of the eight nodes, to
/// the message vectors `$k0`-`$k3`. Nodes n and n + 4 share a register, so that the
/// transposition of each half comes out with the lanes in order.
macro_rules! quarter {
    ($o:literal, $k0:tt, $k1:tt, $k2:tt, $k3:tt) => {
        concat!(
            "vmovdqu xmm8, xmmword ptr [r8 + rsi + ",
            $o,
            "]\n",
            "vmovdqu xmm9, xmmword ptr [r9 + rsi + ",
            $o,
            "]\n",
            "vmovdqu xmm10, xmmword ptr [r10 + rsi + ",
            $o,
            "]\n",
            "vmovdqu xmm11, xmmword ptr [r11 + rsi + ",
            $o,
            "]\n",
            "vinserti128 ymm8, ymm8, xmmword ptr [r12 + rsi + ",
            $o,
            "], 1\n",
            "vinserti128 ymm9, ymm9, xmmword ptr [r13 + rsi + ",
            $o,
            "], 1\n",
            "vinserti128 ymm10, ymm10, xmmword ptr [r14 + rsi + ",
            $o,
            "], 1\n",
            "vinserti128 ymm11, ymm11, xmmword ptr [r15 + rsi + ",
            $o,
            "], 1\n",
            "vpunpckldq ymm12, ymm8, ymm9\n",
            "vpunpckhdq ymm13, ymm8, ymm9\n",
            "vpunpckldq ymm14, ymm10, ymm11\n",
            "vpunpckhdq ymm15, ymm10, ymm11\n",
            "vpunpcklqdq ymm8, ymm12, ymm14\n",
            "vpunpckhqdq ymm9, ymm12, ymm14\n",
            "vpunpcklqdq ymm10, ymm13, ymm15\n",
            "vpunpckhqdq ymm11, ymm13, ymm15\n",
            "vmovdqa ymmword ptr [",
            slot!($k0),
            "], ymm8\n",
            "vmovdqa ymmword ptr [",
            slot!($k1),
            "], ymm9\n",
            "vmovdqa ymmword ptr [",
            slot!($k2),
            "], ymm10\n",
            "vmovdqa ymmword ptr [",
            slot!($k3),
            "], ymm11\n",
        )
    };
}

/// What the assembly reads besides the input, in units of 32 bytes from where rdx
/// points: before it, the starting values of state words 8-11; at it, the place
/// where the rounds keep word 8; then the starting values of words 12-14; the length
/// of a node; from `FLAGS` on, 64 bytes apart, word 15 of each block.
const BASE: usize = 4;
const FLAGS: usize = 10;
const SCRATCH: usize = BASE + FLAGS + 2 * 16;

/// Compresses the eight nodes in `input`, the first of which counts `counter`, and
/// writes their chaining values to `out`.
///
/// The loop over the blocks is assembly as a whole: the chaining values stay in
/// ymm0-ymm7 from block to block, and the message is transposed in the other eight
/// registers before the rounds need them. Not inlined, so that the program holds
/// this long stretch of code once.
#[target_feature(enable = "avx2")]
#[inline(never)]
unsafe fn compress_group(batch: &Batch, input: &[u8], counter: u64, out: &mut [u8]) {
    assert!(input.len() == 8 * batch.node_len && out.len() == 8 * 32);
    let blocks = batch.node_len / 64;
    assert!((1..=16).contains(&blocks));
    let (mut low, mut high) = ([0u32; 8], [0u32; 8]);
    for lane in 0..8 {
        let count = counter + if batch.increment { lane as u64 } else { 0 };
        low[lane] = count as u32;
        high[lane] = (count >> 32) as u32;
    }
    // every load stays inside `input` and the store inside `out`, whose lengths were
    // just checked, except the prefetches, which may name any address; the assembly
    // reads of `message` and `scratch` only what was written before, and writes
    // only `message` and the vector of `scratch` that rdx points at
    unsafe {
        let mut message = MaybeUninit::<Vectors<16>>::uninit();
        let mut scratch = MaybeUninit::<Vectors<SCRATCH>>::uninit();
        let scratch: *mut __m256i = scratch.as_mut_ptr().cast::<__m256i>().add(BASE);
        for (word, value) in IV[..4].iter().enumerate() {
            scratch
                .sub(4 - word)
                .write(_mm256_set1_epi32(*value as i32));
        }
        scratch.add(1).write(load(low.as_ptr().cast()));
        scratch.add(2).write(load(high.as_ptr().cast()));
        scratch.add(3).write(_mm256_set1_epi32(64));
        scratch.add(8).cast::<u64>().write(batch.node_len as u64);
        for block in 0..blocks {
            let start = if block == 0 { batch.flags_start } else { 0 };
            let end = if block + 1 == blocks {
                batch.flags_end
            } else {
                0
            };
            let flags = _mm256_set1_epi32((batch.flags | start | end) as i32);
            scratch.add(FLAGS + 2 * block).write(flags);
        }
        let key = batch.key;
        let mut h: [__m256i; 8] = std::array::from_fn(|word| _mm256_set1_epi32(key[word] as i32));
        let nodes = input.as_ptr();
        let message: *mut __m256i = message.as_mut_ptr().cast();
        std::arch::asm!(
            "2:\n",
            // the lanes are read side by side, too many streams for the processor
            // to fetch ahead on its own
            "prefetcht0 byte ptr [r8 + rsi + 256]\n",
            "prefetcht0 byte ptr [r9 + rsi + 256]\n",
            "prefetcht0 byte ptr [r10 + rsi + 256]\n",
            "prefetcht0 byte ptr [r11 + rsi + 256]\n",
            "prefetcht0 byte ptr [r12 + rsi + 256]\n",
            "prefetcht0 byte ptr [r13 + rsi + 256]\n",
            "prefetcht0 byte ptr [r14 + rsi + 256]\n",
            "prefetcht0 byte ptr [r15 + rsi + 256]\n",
            quarter!("0", 0, 1, 2, 3),
            quarter!("16", 4, 5, 6, 7),
            quarter!("32", 8, 9, 10, 11),
            quarter!("48", 12, 13, 14, 15),
            first_round!(),
            round!(2, 6, 3, 10, 7, 0, 4, 13, 1, 11, 12, 5, 9, 14, 15, 8),
            round!(3, 4, 10, 12, 13, 2, 7, 14, 6, 5, 9, 0, 11, 15, 8, 1),
            round!(10, 7, 12, 9, 14, 3, 13, 15, 4, 0, 11, 2, 5, 8, 1, 6),
            round!(12, 13, 9, 11, 15, 10, 14, 8, 7, 2, 5, 3, 0, 1, 6, 4),
            round!(9, 14, 11, 5, 8, 12, 15, 1, 13, 3, 0, 10, 2, 6, 4, 7),
            round!(11, 15, 5, 0, 1, 9, 8, 6, 14, 10, 2, 12, 3, 4, 7, 13),
            // the new chaining value is the upper half folded onto the lower
            "vpxor ymm0, ymm0, ymmword ptr [rdx]\n",
            "vpxor ymm1, ymm1, ymm9\n",
            "vpxor ymm2, ymm2, ymm10\n",
            "vpxor ymm3, ymm3, ymm11\n",
            "vpxor ymm4, ymm4, ymm12\n",
            "vpxor ymm5, ymm5, ymm13\n",
            "vpxor ymm6, ymm6, ymm14\n",
            "vpxor ymm7, ymm7, ymm15\n",
            "add rsi, 64\n",
            "cmp rsi, qword ptr [rdx + 256]\n",
            "jne 2b\n",
            in("r8") nodes,
            in("r9") nodes.add(batch.node_len),
            in("r10") nodes.add(2 * batch.node_len),
            in("r11") nodes.add(3 * batch.node_len),
            in("r12") nodes.add(4 * batch.node_len),
            in("r13") nodes.add(5 * batch.node_len),
            in("r14") nodes.add(6 * batch.node_len),
            in("r15") nodes.add(7 * batch.node_len),
            inout("rsi") 0usize => _,
            in("rdi") message.add(4),
            in("rax") message.add(12),
            in("rdx") scratch,
            in("rcx") &raw const ROTATIONS,
            inout("ymm0") h[0],
            inout("ymm1") h[1],
            inout("ymm2") h[2],
            inout("ymm3") h[3],
            inout("ymm4") h[4],
            inout("ymm5") h[5],
            inout("ymm6") h[6],
            inout("ymm7") h[7],
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
        for (lane, cv) in transpose(h).into_iter().enumerate() {
            _mm256_storeu_si256(out.as_mut_ptr().add(lane * 32).cast(), cv);
        }
    }
}

/// Compresses the nodes in `input` eight at a time, then four, then two, and
/// returns what is left of the input and of `out`, and the counter of the next node.
#[target_feature(enable = "avx2")]
pub(super) unsafe fn hash_many<'a, 'b>(
    batch: &Batch,
    input: &'a [u8],
    mut counter: u64,
    out: &'b mut [u8],
) -> (&'a [u8], u64, &'b mut [u8]) {
    let (mut rest, mut rest_out) = (input, out);
    // avx2 is enabled for this function
    unsafe {
        while rest.len() >= 8 * batch.node_len {
            let (group, after) = rest.split_at(8 * batch.node_len);
            let (group_out, after_out) = rest_out.split_at_mut(8 * 32);
            compress_group(batch, group, counter, group_out);
            counter += 8 * batch.increment as u64;
            (rest, rest_out) = (after, after_out);
        }
        if rest.len() >= 4 * batch.node_len {
            let (four, after) = rest.split_at(4 * batch.node_len);
            let (four_out, after_out) = rest_out.split_at_mut(4 * 32);
            pairs::compress_four(batch, four, counter, four_out);
            counter += 4 * batch.increment as u64;
            (rest, rest_out) = (after, after_out);
        }
        if rest.len() >= 2 * batch.node_len {
            let (two, after) = rest.split_at(2 * batch.node_len);
            let (two_out, after_out) = rest_out.split_at_mut(2 * 32);
            pairs::compress_two(batch, two, counter, two_out);
            counter += 2 * batch.increment as u64;
            (rest, rest_out) = (after, after_out);
        }
        (rest, counter, rest_out)
    }
}
