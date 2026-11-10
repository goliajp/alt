//! Eight lanes in 256-bit vectors.
//!
//! The rounds are assembly: sixteen state vectors and a temporary do not fit in the
//! sixteen registers, and the compiler's choice of what to keep in memory costs a
//! sixth of the speed. Here fifteen state vectors stay in registers, the sixteenth
//! lives in memory with its register serving as the temporary, and message words are
//! read straight from memory.

use super::IV;
use super::lanes::{Batch, Lanes};
use super::sse41;
use std::arch::x86_64::*;

#[macro_use]
mod rounds;

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

/// eight words from each of eight lanes, as one vector per word
#[inline(always)]
unsafe fn half(blocks: *const u8, stride: usize) -> [__m256i; 8] {
    unsafe {
        transpose([
            load(blocks),
            load(blocks.add(stride)),
            load(blocks.add(2 * stride)),
            load(blocks.add(3 * stride)),
            load(blocks.add(4 * stride)),
            load(blocks.add(5 * stride)),
            load(blocks.add(6 * stride)),
            load(blocks.add(7 * stride)),
        ])
    }
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

/// Compresses the eight nodes in `input`, the first of which counts `counter`, and
/// writes their chaining values to `out`.
#[target_feature(enable = "avx2")]
unsafe fn compress_group(batch: &Batch, input: &[u8], counter: u64, out: &mut [u8]) {
    assert!(input.len() == 8 * batch.node_len && out.len() == 8 * 32);
    let (mut low, mut high) = ([0u32; 8], [0u32; 8]);
    for lane in 0..8 {
        let count = counter + if batch.increment { lane as u64 } else { 0 };
        low[lane] = count as u32;
        high[lane] = (count >> 32) as u32;
    }
    let blocks = batch.node_len / 64;
    // every load stays inside `input` and the store inside `out`, whose lengths were
    // just checked; the assembly reads and writes only the three arrays it is given
    unsafe {
        let key = batch.key;
        let mut h = Vectors(std::array::from_fn(|word| {
            _mm256_set1_epi32(key[word] as i32)
        }));
        let (low, high) = (load(low.as_ptr().cast()), load(high.as_ptr().cast()));
        let mut message = Vectors([_mm256_setzero_si256(); 16]);
        let mut flags = batch.flags | batch.flags_start;
        for block in 0..blocks {
            if block + 1 == blocks {
                flags |= batch.flags_end;
            }
            let at = input.as_ptr().add(block * 64);
            // lanes are read side by side, too many streams for the processor to
            // fetch ahead on its own; an address past the end is harmless
            for lane in 0..8 {
                _mm_prefetch::<_MM_HINT_T0>(at.wrapping_add(lane * batch.node_len + 256).cast());
            }
            let (first, second) = message.0.split_at_mut(8);
            first.copy_from_slice(&half(at, batch.node_len));
            second.copy_from_slice(&half(at.add(32), batch.node_len));
            let mut rest = Vectors([
                _mm256_set1_epi32(IV[0] as i32),
                _mm256_set1_epi32(IV[1] as i32),
                _mm256_set1_epi32(IV[2] as i32),
                _mm256_set1_epi32(IV[3] as i32),
                low,
                high,
                _mm256_set1_epi32(64),
                _mm256_set1_epi32(flags as i32),
            ]);
            std::arch::asm!(
                "vmovdqa ymm0, ymmword ptr [rsi]\n",
                "vmovdqa ymm1, ymmword ptr [rsi + 32]\n",
                "vmovdqa ymm2, ymmword ptr [rsi + 64]\n",
                "vmovdqa ymm3, ymmword ptr [rsi + 96]\n",
                "vmovdqa ymm4, ymmword ptr [rsi + 128]\n",
                "vmovdqa ymm5, ymmword ptr [rsi + 160]\n",
                "vmovdqa ymm6, ymmword ptr [rsi + 192]\n",
                "vmovdqa ymm7, ymmword ptr [rsi + 224]\n",
                "vmovdqa ymm9, ymmword ptr [rdx + 32]\n",
                "vmovdqa ymm10, ymmword ptr [rdx + 64]\n",
                "vmovdqa ymm11, ymmword ptr [rdx + 96]\n",
                "vmovdqa ymm12, ymmword ptr [rdx + 128]\n",
                "vmovdqa ymm13, ymmword ptr [rdx + 160]\n",
                "vmovdqa ymm14, ymmword ptr [rdx + 192]\n",
                "vmovdqa ymm15, ymmword ptr [rdx + 224]\n",
                round!(0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15),
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
                "vmovdqa ymmword ptr [rsi], ymm0\n",
                "vmovdqa ymmword ptr [rsi + 32], ymm1\n",
                "vmovdqa ymmword ptr [rsi + 64], ymm2\n",
                "vmovdqa ymmword ptr [rsi + 96], ymm3\n",
                "vmovdqa ymmword ptr [rsi + 128], ymm4\n",
                "vmovdqa ymmword ptr [rsi + 160], ymm5\n",
                "vmovdqa ymmword ptr [rsi + 192], ymm6\n",
                "vmovdqa ymmword ptr [rsi + 224], ymm7\n",
                in("rdi") message.0.as_ptr(),
                in("rsi") h.0.as_mut_ptr(),
                in("rdx") rest.0.as_mut_ptr(),
                in("rcx") &raw const ROTATIONS,
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
            flags = batch.flags;
        }
        for (lane, cv) in transpose(h.0).into_iter().enumerate() {
            _mm256_storeu_si256(out.as_mut_ptr().add(lane * 32).cast(), cv);
        }
    }
}

#[repr(C, align(16))]
struct Quarter<const N: usize>([__m128i; N]);

/// Compresses the four nodes in `input`, the first of which counts `counter`, and
/// writes their chaining values to `out`: the same rounds in the lower halves of the
/// registers.
#[target_feature(enable = "avx2")]
unsafe fn compress_four(batch: &Batch, input: &[u8], counter: u64, out: &mut [u8]) {
    assert!(input.len() == 4 * batch.node_len && out.len() == 4 * 32);
    let (mut low, mut high) = ([0u32; 8], [0u32; 8]);
    for lane in 0..4 {
        let count = counter + if batch.increment { lane as u64 } else { 0 };
        low[lane] = count as u32;
        high[lane] = (count >> 32) as u32;
    }
    let blocks = batch.node_len / 64;
    // every load stays inside `input` and the store inside `out`, whose lengths were
    // just checked; the assembly reads and writes only the three arrays it is given
    unsafe {
        let key = batch.key;
        let mut h = Quarter(std::array::from_fn(|word| _mm_set1_epi32(key[word] as i32)));
        let (low, high) = (
            <__m128i as Lanes>::words(&low),
            <__m128i as Lanes>::words(&high),
        );
        let mut flags = batch.flags | batch.flags_start;
        for block in 0..blocks {
            if block + 1 == blocks {
                flags |= batch.flags_end;
            }
            let message = Quarter(<__m128i as Lanes>::load(
                input.as_ptr().add(block * 64),
                batch.node_len,
            ));
            let mut rest = Quarter([
                _mm_set1_epi32(IV[0] as i32),
                _mm_set1_epi32(IV[1] as i32),
                _mm_set1_epi32(IV[2] as i32),
                _mm_set1_epi32(IV[3] as i32),
                low,
                high,
                _mm_set1_epi32(64),
                _mm_set1_epi32(flags as i32),
            ]);
            std::arch::asm!(
                "vmovdqa xmm0, xmmword ptr [rsi]\n",
                "vmovdqa xmm1, xmmword ptr [rsi + 16]\n",
                "vmovdqa xmm2, xmmword ptr [rsi + 32]\n",
                "vmovdqa xmm3, xmmword ptr [rsi + 48]\n",
                "vmovdqa xmm4, xmmword ptr [rsi + 64]\n",
                "vmovdqa xmm5, xmmword ptr [rsi + 80]\n",
                "vmovdqa xmm6, xmmword ptr [rsi + 96]\n",
                "vmovdqa xmm7, xmmword ptr [rsi + 112]\n",
                "vmovdqa xmm9, xmmword ptr [rdx + 16]\n",
                "vmovdqa xmm10, xmmword ptr [rdx + 32]\n",
                "vmovdqa xmm11, xmmword ptr [rdx + 48]\n",
                "vmovdqa xmm12, xmmword ptr [rdx + 64]\n",
                "vmovdqa xmm13, xmmword ptr [rdx + 80]\n",
                "vmovdqa xmm14, xmmword ptr [rdx + 96]\n",
                "vmovdqa xmm15, xmmword ptr [rdx + 112]\n",
                round_four!(0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15),
                round_four!(2, 6, 3, 10, 7, 0, 4, 13, 1, 11, 12, 5, 9, 14, 15, 8),
                round_four!(3, 4, 10, 12, 13, 2, 7, 14, 6, 5, 9, 0, 11, 15, 8, 1),
                round_four!(10, 7, 12, 9, 14, 3, 13, 15, 4, 0, 11, 2, 5, 8, 1, 6),
                round_four!(12, 13, 9, 11, 15, 10, 14, 8, 7, 2, 5, 3, 0, 1, 6, 4),
                round_four!(9, 14, 11, 5, 8, 12, 15, 1, 13, 3, 0, 10, 2, 6, 4, 7),
                round_four!(11, 15, 5, 0, 1, 9, 8, 6, 14, 10, 2, 12, 3, 4, 7, 13),
                "vpxor xmm0, xmm0, xmmword ptr [rdx]\n",
                "vpxor xmm1, xmm1, xmm9\n",
                "vpxor xmm2, xmm2, xmm10\n",
                "vpxor xmm3, xmm3, xmm11\n",
                "vpxor xmm4, xmm4, xmm12\n",
                "vpxor xmm5, xmm5, xmm13\n",
                "vpxor xmm6, xmm6, xmm14\n",
                "vpxor xmm7, xmm7, xmm15\n",
                "vmovdqa xmmword ptr [rsi], xmm0\n",
                "vmovdqa xmmword ptr [rsi + 16], xmm1\n",
                "vmovdqa xmmword ptr [rsi + 32], xmm2\n",
                "vmovdqa xmmword ptr [rsi + 48], xmm3\n",
                "vmovdqa xmmword ptr [rsi + 64], xmm4\n",
                "vmovdqa xmmword ptr [rsi + 80], xmm5\n",
                "vmovdqa xmmword ptr [rsi + 96], xmm6\n",
                "vmovdqa xmmword ptr [rsi + 112], xmm7\n",
                in("rdi") message.0.as_ptr(),
                in("rsi") h.0.as_mut_ptr(),
                in("rdx") rest.0.as_mut_ptr(),
                in("rcx") &raw const ROTATIONS,
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
            flags = batch.flags;
        }
        <__m128i as Lanes>::store(&h.0, out.as_mut_ptr());
    }
}

/// Compresses the nodes in `input` eight at a time, then four at a time, and
/// returns what is left of the input and of `out`, and the counter of the next node.
#[target_feature(enable = "avx2")]
pub(super) unsafe fn hash_many<'a, 'b>(
    batch: &Batch,
    input: &'a [u8],
    mut counter: u64,
    out: &'b mut [u8],
) -> (&'a [u8], u64, &'b mut [u8]) {
    let groups = input.len() / (8 * batch.node_len);
    let (wide, rest) = input.split_at(groups * 8 * batch.node_len);
    let (wide_out, rest_out) = out.split_at_mut(groups * 8 * 32);
    let wide = wide.chunks_exact(8 * batch.node_len);
    // avx2 is enabled for this function, and it implies sse4.1
    unsafe {
        for (group, out) in wide.zip(wide_out.as_chunks_mut::<{ 8 * 32 }>().0) {
            compress_group(batch, group, counter, out);
            counter += 8 * batch.increment as u64;
        }
        let (mut rest, mut rest_out) = (rest, rest_out);
        if rest.len() >= 4 * batch.node_len {
            let (four, after) = rest.split_at(4 * batch.node_len);
            let (four_out, after_out) = rest_out.split_at_mut(4 * 32);
            compress_four(batch, four, counter, four_out);
            counter += 4 * batch.increment as u64;
            (rest, rest_out) = (after, after_out);
        }
        sse41::hash_many(batch, rest, counter, rest_out)
    }
}
