//! SHA-1 of many independent messages at once.
//!
//! On x86-64 with AVX2 eight messages advance together, one 64-byte block each per
//! step, in the eight 32-bit lanes of a 256-bit register. Elsewhere, and for the
//! tail of a batch, each message is hashed on its own with the fastest
//! single-stream implementation.

use crate::sha1::Sha1;

/// one message given as consecutive parts (for example an object header and its body)
pub type Parts<'a> = &'a [&'a [u8]];

/// SHA-1 of every message, in input order
pub fn sha1_many(messages: &[Parts<'_>]) -> Vec<[u8; 20]> {
    #[cfg(target_arch = "x86_64")]
    if messages.len() >= 4 && std::arch::is_x86_feature_detected!("avx2") {
        // the feature was just detected on this CPU
        return unsafe { avx2::many(messages) };
    }
    messages.iter().map(|parts| one(parts)).collect()
}

fn one(parts: Parts<'_>) -> [u8; 20] {
    let mut hasher = Sha1::new();
    for part in parts {
        hasher.update(part);
    }
    hasher.finalize()
}

#[cfg(target_arch = "x86_64")]
mod avx2 {
    use super::Parts;
    use std::arch::x86_64::*;

    const INITIAL: [u32; 5] = [
        0x6745_2301,
        0xefcd_ab89,
        0x98ba_dcfe,
        0x1032_5476,
        0xc3d2_e1f0,
    ];
    static K: [u32; 4] = [0x5a82_7999, 0x6ed9_eba1, 0x8f1b_bcdc, 0xca62_c1d6];

    /// one lane's position inside its message and its padding
    struct Lane<'a> {
        job: usize,
        parts: Parts<'a>,
        part: usize,
        offset: usize,
        bytes: u64,
        /// 0: data remains, 1: the length block is still owed, 2: finished
        stage: u8,
    }

    impl Lane<'_> {
        /// The next block: a pointer straight into the message while whole blocks remain,
        /// otherwise the padded tail written to `scratch`. None once the message is exhausted.
        fn next(&mut self, scratch: &mut [u8; 64]) -> Option<*const u8> {
            if self.stage == 2 {
                return None;
            }
            let mut filled = 0;
            if self.stage == 0 {
                if self.part < self.parts.len() && self.parts[self.part].len() - self.offset >= 64 {
                    let block = self.parts[self.part][self.offset..].as_ptr();
                    self.offset += 64;
                    if self.offset == self.parts[self.part].len() {
                        self.part += 1;
                        self.offset = 0;
                    }
                    return Some(block);
                }
                while filled < 64 && self.part < self.parts.len() {
                    let rest = &self.parts[self.part][self.offset..];
                    let take = rest.len().min(64 - filled);
                    scratch[filled..filled + take].copy_from_slice(&rest[..take]);
                    filled += take;
                    self.offset += take;
                    if self.offset == self.parts[self.part].len() {
                        self.part += 1;
                        self.offset = 0;
                    }
                }
                if filled == 64 {
                    return Some(scratch.as_ptr());
                }
                scratch[filled] = 0x80;
                filled += 1;
            }
            scratch[filled..].fill(0);
            if filled <= 56 {
                scratch[56..].copy_from_slice(&self.bytes.wrapping_mul(8).to_be_bytes());
                self.stage = 2;
            } else {
                self.stage = 1;
            }
            Some(scratch.as_ptr())
        }
    }

    /// Turns eight rows of eight big-endian words (one row per lane) into eight
    /// registers of one word position each.
    #[inline(always)]
    unsafe fn transpose(rows: [__m256i; 8]) -> [__m256i; 8] {
        unsafe {
            let swap = _mm256_setr_epi8(
                3, 2, 1, 0, 7, 6, 5, 4, 11, 10, 9, 8, 15, 14, 13, 12, 3, 2, 1, 0, 7, 6, 5, 4, 11,
                10, 9, 8, 15, 14, 13, 12,
            );
            let r = rows.map(|row| _mm256_shuffle_epi8(row, swap));
            let t0 = _mm256_unpacklo_epi32(r[0], r[1]);
            let t1 = _mm256_unpackhi_epi32(r[0], r[1]);
            let t2 = _mm256_unpacklo_epi32(r[2], r[3]);
            let t3 = _mm256_unpackhi_epi32(r[2], r[3]);
            let t4 = _mm256_unpacklo_epi32(r[4], r[5]);
            let t5 = _mm256_unpackhi_epi32(r[4], r[5]);
            let t6 = _mm256_unpacklo_epi32(r[6], r[7]);
            let t7 = _mm256_unpackhi_epi32(r[6], r[7]);
            let u0 = _mm256_unpacklo_epi64(t0, t2);
            let u1 = _mm256_unpackhi_epi64(t0, t2);
            let u2 = _mm256_unpacklo_epi64(t1, t3);
            let u3 = _mm256_unpackhi_epi64(t1, t3);
            let u4 = _mm256_unpacklo_epi64(t4, t6);
            let u5 = _mm256_unpackhi_epi64(t4, t6);
            let u6 = _mm256_unpacklo_epi64(t5, t7);
            let u7 = _mm256_unpackhi_epi64(t5, t7);
            [
                _mm256_permute2x128_si256::<0x20>(u0, u4),
                _mm256_permute2x128_si256::<0x20>(u1, u5),
                _mm256_permute2x128_si256::<0x20>(u2, u6),
                _mm256_permute2x128_si256::<0x20>(u3, u7),
                _mm256_permute2x128_si256::<0x31>(u0, u4),
                _mm256_permute2x128_si256::<0x31>(u1, u5),
                _mm256_permute2x128_si256::<0x31>(u2, u6),
                _mm256_permute2x128_si256::<0x31>(u3, u7),
            ]
        }
    }

    /// the states of the eight lanes, one row per state word, and the eighty schedule
    /// words of the current step, one row per word
    #[repr(C, align(32))]
    struct Context {
        state: [[u32; 8]; 5],
        words: [[u32; 8]; 80],
    }

    /// One of the schedule words 16..32, at `$at` bytes from the cursor in rax:
    /// w[t] = rol(w[t-3] ^ w[t-8] ^ w[t-14] ^ w[t-16], 1)
    macro_rules! early {
        ($at:literal) => {
            concat!(
                "vmovdqa ymm5, ymmword ptr [rax + ",
                $at,
                " - 96]\n",
                "vpxor ymm5, ymm5, ymmword ptr [rax + ",
                $at,
                " - 256]\n",
                "vpxor ymm5, ymm5, ymmword ptr [rax + ",
                $at,
                " - 448]\n",
                "vpxor ymm5, ymm5, ymmword ptr [rax + ",
                $at,
                " - 512]\n",
                "vpaddd ymm6, ymm5, ymm5\n",
                "vpsrld ymm5, ymm5, 31\n",
                "vpor ymm5, ymm5, ymm6\n",
                "vmovdqa ymmword ptr [rax + ",
                $at,
                "], ymm5\n",
            )
        };
    }

    /// One of the schedule words 32..80, which reaches further back and so does not
    /// wait for the words just before it:
    /// w[t] = rol(w[t-6] ^ w[t-16] ^ w[t-28] ^ w[t-32], 2)
    macro_rules! late {
        ($at:literal) => {
            concat!(
                "vmovdqa ymm5, ymmword ptr [rax + ",
                $at,
                " - 192]\n",
                "vpxor ymm5, ymm5, ymmword ptr [rax + ",
                $at,
                " - 512]\n",
                "vpxor ymm5, ymm5, ymmword ptr [rax + ",
                $at,
                " - 896]\n",
                "vpxor ymm5, ymm5, ymmword ptr [rax + ",
                $at,
                " - 1024]\n",
                "vpslld ymm6, ymm5, 2\n",
                "vpsrld ymm5, ymm5, 30\n",
                "vpor ymm5, ymm5, ymm6\n",
                "vmovdqa ymmword ptr [rax + ",
                $at,
                "], ymm5\n",
            )
        };
    }

    /// the round function of (b, c, d) into ymm5
    macro_rules! function {
        (choose, $b:literal, $c:literal, $d:literal) => {
            concat!(
                "vpxor ymm5, ",
                $c,
                ", ",
                $d,
                "\n",
                "vpand ymm5, ymm5, ",
                $b,
                "\n",
                "vpxor ymm5, ymm5, ",
                $d,
                "\n",
            )
        };
        (parity, $b:literal, $c:literal, $d:literal) => {
            concat!(
                "vpxor ymm5, ",
                $b,
                ", ",
                $c,
                "\n",
                "vpxor ymm5, ymm5, ",
                $d,
                "\n",
            )
        };
        (majority, $b:literal, $c:literal, $d:literal) => {
            concat!(
                "vpor ymm5, ",
                $b,
                ", ",
                $c,
                "\n",
                "vpand ymm5, ymm5, ",
                $d,
                "\n",
                "vpand ymm6, ",
                $b,
                ", ",
                $c,
                "\n",
                "vpor ymm5, ymm5, ymm6\n",
            )
        };
    }

    /// One round for all eight lanes, on the word at `$at` bytes from the cursor. The
    /// word, the constant in ymm7 and the round function are added to `e` first; the
    /// rotation of `a`, the value computed last, joins at the end.
    macro_rules! round {
        ($f:ident, $a:literal, $b:literal, $c:literal, $d:literal, $e:literal, $at:literal) => {
            concat!(
                function!($f, $b, $c, $d),
                "vpaddd ",
                $e,
                ", ",
                $e,
                ", ymmword ptr [rax + ",
                $at,
                "]\n",
                "vpaddd ",
                $e,
                ", ",
                $e,
                ", ymm7\n",
                "vpaddd ",
                $e,
                ", ",
                $e,
                ", ymm5\n",
                "vpslld ymm5, ",
                $a,
                ", 5\n",
                "vpsrld ymm6, ",
                $a,
                ", 27\n",
                "vpor ymm5, ymm5, ymm6\n",
                "vpaddd ",
                $e,
                ", ",
                $e,
                ", ymm5\n",
                "vpslld ymm5, ",
                $b,
                ", 30\n",
                "vpsrld ",
                $b,
                ", ",
                $b,
                ", 2\n",
                "vpor ",
                $b,
                ", ",
                $b,
                ", ymm5\n",
            )
        };
    }

    /// Twenty rounds with one function and one constant, as a loop of five: after five
    /// rounds the working variables are back in their registers.
    macro_rules! twenty {
        ($f:ident, $constant:literal, $label:literal) => {
            concat!(
                "vpbroadcastd ymm7, dword ptr [r10 + ",
                $constant,
                "]\n",
                "lea rcx, [rax + 640]\n",
                $label,
                ":\n",
                round!($f, "ymm0", "ymm1", "ymm2", "ymm3", "ymm4", 0),
                round!($f, "ymm4", "ymm0", "ymm1", "ymm2", "ymm3", 32),
                round!($f, "ymm3", "ymm4", "ymm0", "ymm1", "ymm2", 64),
                round!($f, "ymm2", "ymm3", "ymm4", "ymm0", "ymm1", 96),
                round!($f, "ymm1", "ymm2", "ymm3", "ymm4", "ymm0", 128),
                "add rax, 160\n",
                "cmp rax, rcx\n",
                "jne ",
                $label,
                "b\n",
            )
        };
    }

    /// Folds one block per lane into the eight states. Everything after the load is a
    /// handful of short loops, which the processor keeps decoded; the rounds are
    /// assembly because the compiler reorders their sums.
    #[target_feature(enable = "avx2")]
    unsafe fn compress(context: &mut Context, blocks: &[*const u8; 8]) {
        unsafe {
            let low = transpose(blocks.map(|block| _mm256_loadu_si256(block.cast())));
            let high = transpose(blocks.map(|block| _mm256_loadu_si256(block.add(32).cast())));
            for (row, word) in context.words.iter_mut().zip(low.into_iter().chain(high)) {
                _mm256_store_si256(row.as_mut_ptr().cast(), word);
            }
            std::arch::asm!(
                "lea rax, [r11 + 160 + 512]\n",
                "lea rcx, [rax + 512]\n",
                "2:\n",
                early!(0),
                early!(32),
                early!(64),
                early!(96),
                "add rax, 128\n",
                "cmp rax, rcx\n",
                "jne 2b\n",
                "lea rcx, [rax + 1536]\n",
                "3:\n",
                late!(0),
                late!(32),
                late!(64),
                late!(96),
                "add rax, 128\n",
                "cmp rax, rcx\n",
                "jne 3b\n",
                "vmovdqa ymm0, ymmword ptr [r11]\n",
                "vmovdqa ymm1, ymmword ptr [r11 + 32]\n",
                "vmovdqa ymm2, ymmword ptr [r11 + 64]\n",
                "vmovdqa ymm3, ymmword ptr [r11 + 96]\n",
                "vmovdqa ymm4, ymmword ptr [r11 + 128]\n",
                "lea rax, [r11 + 160]\n",
                twenty!(choose, 0, "4"),
                twenty!(parity, 4, "5"),
                twenty!(majority, 8, "6"),
                twenty!(parity, 12, "7"),
                "vpaddd ymm0, ymm0, ymmword ptr [r11]\n",
                "vpaddd ymm1, ymm1, ymmword ptr [r11 + 32]\n",
                "vpaddd ymm2, ymm2, ymmword ptr [r11 + 64]\n",
                "vpaddd ymm3, ymm3, ymmword ptr [r11 + 96]\n",
                "vpaddd ymm4, ymm4, ymmword ptr [r11 + 128]\n",
                "vmovdqa ymmword ptr [r11], ymm0\n",
                "vmovdqa ymmword ptr [r11 + 32], ymm1\n",
                "vmovdqa ymmword ptr [r11 + 64], ymm2\n",
                "vmovdqa ymmword ptr [r11 + 96], ymm3\n",
                "vmovdqa ymmword ptr [r11 + 128], ymm4\n",
                in("r11") context as *mut Context,
                in("r10") K.as_ptr(),
                out("rax") _,
                out("rcx") _,
                out("ymm0") _,
                out("ymm1") _,
                out("ymm2") _,
                out("ymm3") _,
                out("ymm4") _,
                out("ymm5") _,
                out("ymm6") _,
                out("ymm7") _,
                options(nostack),
            );
        }
    }

    #[target_feature(enable = "avx2")]
    pub(super) unsafe fn many(messages: &[Parts<'_>]) -> Vec<[u8; 20]> {
        unsafe {
            let mut out = vec![[0u8; 20]; messages.len()];
            // longest first, so the lanes of one step have similar amounts left
            let lengths: Vec<u64> = messages
                .iter()
                .map(|parts| parts.iter().map(|part| part.len() as u64).sum())
                .collect();
            let mut order: Vec<usize> = (0..messages.len()).collect();
            order.sort_unstable_by_key(|job| std::cmp::Reverse(lengths[*job]));
            let mut queue = order.into_iter();
            let mut context = Context {
                state: [[0; 8]; 5],
                words: [[0; 8]; 80],
            };
            let mut lanes: [Option<Lane<'_>>; 8] = Default::default();
            let mut scratch = [[0u8; 64]; 8];
            let idle = [0u8; 64];
            let mut blocks = [idle.as_ptr(); 8];
            loop {
                let mut active = false;
                for index in 0..8 {
                    loop {
                        if let Some(lane) = &mut lanes[index]
                            && let Some(block) = lane.next(&mut scratch[index])
                        {
                            blocks[index] = block;
                            active = true;
                            break;
                        }
                        if let Some(lane) = lanes[index].take() {
                            // this lane's message is complete: read its digest out
                            for (chunk, row) in out[lane.job]
                                .as_chunks_mut::<4>()
                                .0
                                .iter_mut()
                                .zip(context.state)
                            {
                                chunk.copy_from_slice(&row[index].to_be_bytes());
                            }
                        }
                        let Some(job) = queue.next() else {
                            blocks[index] = idle.as_ptr();
                            break;
                        };
                        // start the next message in this lane from the initial state
                        for (row, initial) in context.state.iter_mut().zip(INITIAL) {
                            row[index] = initial;
                        }
                        lanes[index] = Some(Lane {
                            job,
                            parts: messages[job],
                            part: 0,
                            offset: 0,
                            bytes: lengths[job],
                            stage: 0,
                        });
                    }
                }
                if !active {
                    break;
                }
                // an idle lane folds zeros; its state is reset before it is used again
                compress(&mut context, &blocks);
            }
            out
        }
    }
}
