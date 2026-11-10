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
    const K: [u32; 4] = [0x5a82_7999, 0x6ed9_eba1, 0x8f1b_bcdc, 0xca62_c1d6];

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

    #[inline(always)]
    unsafe fn rotate<const LEFT: i32, const RIGHT: i32>(value: __m256i) -> __m256i {
        unsafe {
            _mm256_or_si256(
                _mm256_slli_epi32::<LEFT>(value),
                _mm256_srli_epi32::<RIGHT>(value),
            )
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

    #[inline(always)]
    unsafe fn schedule(w: &mut [__m256i; 16], t: usize) -> __m256i {
        unsafe {
            let mixed = _mm256_xor_si256(
                _mm256_xor_si256(w[(t + 13) & 15], w[(t + 8) & 15]),
                _mm256_xor_si256(w[(t + 2) & 15], w[t & 15]),
            );
            w[t & 15] = rotate::<1, 31>(mixed);
            w[t & 15]
        }
    }

    /// the round function of (b, c, d) into ymm5
    macro_rules! function_asm {
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
                "\n"
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
                "\n"
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
                "vpor ymm5, ymm5, ymm6\n"
            )
        };
    }

    /// One round for all eight lanes. The word (constant included) and the round
    /// function are added to `e` first; the rotation of `a`, the value computed last,
    /// joins at the end. Written as assembly because the compiler reorders these sums.
    macro_rules! round_asm {
        ($f:ident, $a:literal, $b:literal, $c:literal, $d:literal, $e:literal, $t:literal) => {
            concat!(
                function_asm!($f, $b, $c, $d),
                "vpaddd ",
                $e,
                ", ",
                $e,
                ", ymmword ptr [r11 + ",
                $t,
                "*32]\n",
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
                ", ymm5\n"
            )
        };
    }

    /// schedule word `$t`, stored with its round constant
    macro_rules! extend {
        ($w:ident, $wk:ident, $k:ident, $t:expr) => {
            let word = schedule(&mut $w, $t);
            _mm256_storeu_si256($wk.add($t), _mm256_add_epi32(word, $k[$t / 20]));
        };
    }
    macro_rules! extend_sixteen {
        ($w:ident, $wk:ident, $k:ident, $base:expr) => {
            extend!($w, $wk, $k, $base + 0);
            extend!($w, $wk, $k, $base + 1);
            extend!($w, $wk, $k, $base + 2);
            extend!($w, $wk, $k, $base + 3);
            extend!($w, $wk, $k, $base + 4);
            extend!($w, $wk, $k, $base + 5);
            extend!($w, $wk, $k, $base + 6);
            extend!($w, $wk, $k, $base + 7);
            extend!($w, $wk, $k, $base + 8);
            extend!($w, $wk, $k, $base + 9);
            extend!($w, $wk, $k, $base + 10);
            extend!($w, $wk, $k, $base + 11);
            extend!($w, $wk, $k, $base + 12);
            extend!($w, $wk, $k, $base + 13);
            extend!($w, $wk, $k, $base + 14);
            extend!($w, $wk, $k, $base + 15);
        };
    }

    /// folds one block per lane into the eight interleaved states
    #[target_feature(enable = "avx2")]
    unsafe fn compress(state: &mut [__m256i; 5], blocks: &[*const u8; 8]) {
        unsafe {
            let low = transpose(blocks.map(|block| _mm256_loadu_si256(block.cast())));
            let high = transpose(blocks.map(|block| _mm256_loadu_si256(block.add(32).cast())));
            let mut w = [_mm256_setzero_si256(); 16];
            w[..8].copy_from_slice(&low);
            w[8..].copy_from_slice(&high);
            let k = K.map(|value| _mm256_set1_epi32(value as i32));
            // every word is written here, constant included, before the rounds read it
            let mut words = std::mem::MaybeUninit::<[__m256i; 80]>::uninit();
            let wk = words.as_mut_ptr().cast::<__m256i>();
            for (t, word) in w.iter().enumerate() {
                _mm256_storeu_si256(wk.add(t), _mm256_add_epi32(*word, k[0]));
            }
            // expanded with constant positions: a run-time loop would index the window in
            // memory and chain every word through a store and a reload
            extend_sixteen!(w, wk, k, 16);
            extend_sixteen!(w, wk, k, 32);
            extend_sixteen!(w, wk, k, 48);
            extend_sixteen!(w, wk, k, 64);
            let [mut a, mut b, mut c, mut d, mut e] = *state;
            std::arch::asm!(
                round_asm!(choose, "ymm0", "ymm1", "ymm2", "ymm3", "ymm4", 0),
                round_asm!(choose, "ymm4", "ymm0", "ymm1", "ymm2", "ymm3", 1),
                round_asm!(choose, "ymm3", "ymm4", "ymm0", "ymm1", "ymm2", 2),
                round_asm!(choose, "ymm2", "ymm3", "ymm4", "ymm0", "ymm1", 3),
                round_asm!(choose, "ymm1", "ymm2", "ymm3", "ymm4", "ymm0", 4),
                round_asm!(choose, "ymm0", "ymm1", "ymm2", "ymm3", "ymm4", 5),
                round_asm!(choose, "ymm4", "ymm0", "ymm1", "ymm2", "ymm3", 6),
                round_asm!(choose, "ymm3", "ymm4", "ymm0", "ymm1", "ymm2", 7),
                round_asm!(choose, "ymm2", "ymm3", "ymm4", "ymm0", "ymm1", 8),
                round_asm!(choose, "ymm1", "ymm2", "ymm3", "ymm4", "ymm0", 9),
                round_asm!(choose, "ymm0", "ymm1", "ymm2", "ymm3", "ymm4", 10),
                round_asm!(choose, "ymm4", "ymm0", "ymm1", "ymm2", "ymm3", 11),
                round_asm!(choose, "ymm3", "ymm4", "ymm0", "ymm1", "ymm2", 12),
                round_asm!(choose, "ymm2", "ymm3", "ymm4", "ymm0", "ymm1", 13),
                round_asm!(choose, "ymm1", "ymm2", "ymm3", "ymm4", "ymm0", 14),
                round_asm!(choose, "ymm0", "ymm1", "ymm2", "ymm3", "ymm4", 15),
                round_asm!(choose, "ymm4", "ymm0", "ymm1", "ymm2", "ymm3", 16),
                round_asm!(choose, "ymm3", "ymm4", "ymm0", "ymm1", "ymm2", 17),
                round_asm!(choose, "ymm2", "ymm3", "ymm4", "ymm0", "ymm1", 18),
                round_asm!(choose, "ymm1", "ymm2", "ymm3", "ymm4", "ymm0", 19),
                round_asm!(parity, "ymm0", "ymm1", "ymm2", "ymm3", "ymm4", 20),
                round_asm!(parity, "ymm4", "ymm0", "ymm1", "ymm2", "ymm3", 21),
                round_asm!(parity, "ymm3", "ymm4", "ymm0", "ymm1", "ymm2", 22),
                round_asm!(parity, "ymm2", "ymm3", "ymm4", "ymm0", "ymm1", 23),
                round_asm!(parity, "ymm1", "ymm2", "ymm3", "ymm4", "ymm0", 24),
                round_asm!(parity, "ymm0", "ymm1", "ymm2", "ymm3", "ymm4", 25),
                round_asm!(parity, "ymm4", "ymm0", "ymm1", "ymm2", "ymm3", 26),
                round_asm!(parity, "ymm3", "ymm4", "ymm0", "ymm1", "ymm2", 27),
                round_asm!(parity, "ymm2", "ymm3", "ymm4", "ymm0", "ymm1", 28),
                round_asm!(parity, "ymm1", "ymm2", "ymm3", "ymm4", "ymm0", 29),
                round_asm!(parity, "ymm0", "ymm1", "ymm2", "ymm3", "ymm4", 30),
                round_asm!(parity, "ymm4", "ymm0", "ymm1", "ymm2", "ymm3", 31),
                round_asm!(parity, "ymm3", "ymm4", "ymm0", "ymm1", "ymm2", 32),
                round_asm!(parity, "ymm2", "ymm3", "ymm4", "ymm0", "ymm1", 33),
                round_asm!(parity, "ymm1", "ymm2", "ymm3", "ymm4", "ymm0", 34),
                round_asm!(parity, "ymm0", "ymm1", "ymm2", "ymm3", "ymm4", 35),
                round_asm!(parity, "ymm4", "ymm0", "ymm1", "ymm2", "ymm3", 36),
                round_asm!(parity, "ymm3", "ymm4", "ymm0", "ymm1", "ymm2", 37),
                round_asm!(parity, "ymm2", "ymm3", "ymm4", "ymm0", "ymm1", 38),
                round_asm!(parity, "ymm1", "ymm2", "ymm3", "ymm4", "ymm0", 39),
                round_asm!(majority, "ymm0", "ymm1", "ymm2", "ymm3", "ymm4", 40),
                round_asm!(majority, "ymm4", "ymm0", "ymm1", "ymm2", "ymm3", 41),
                round_asm!(majority, "ymm3", "ymm4", "ymm0", "ymm1", "ymm2", 42),
                round_asm!(majority, "ymm2", "ymm3", "ymm4", "ymm0", "ymm1", 43),
                round_asm!(majority, "ymm1", "ymm2", "ymm3", "ymm4", "ymm0", 44),
                round_asm!(majority, "ymm0", "ymm1", "ymm2", "ymm3", "ymm4", 45),
                round_asm!(majority, "ymm4", "ymm0", "ymm1", "ymm2", "ymm3", 46),
                round_asm!(majority, "ymm3", "ymm4", "ymm0", "ymm1", "ymm2", 47),
                round_asm!(majority, "ymm2", "ymm3", "ymm4", "ymm0", "ymm1", 48),
                round_asm!(majority, "ymm1", "ymm2", "ymm3", "ymm4", "ymm0", 49),
                round_asm!(majority, "ymm0", "ymm1", "ymm2", "ymm3", "ymm4", 50),
                round_asm!(majority, "ymm4", "ymm0", "ymm1", "ymm2", "ymm3", 51),
                round_asm!(majority, "ymm3", "ymm4", "ymm0", "ymm1", "ymm2", 52),
                round_asm!(majority, "ymm2", "ymm3", "ymm4", "ymm0", "ymm1", 53),
                round_asm!(majority, "ymm1", "ymm2", "ymm3", "ymm4", "ymm0", 54),
                round_asm!(majority, "ymm0", "ymm1", "ymm2", "ymm3", "ymm4", 55),
                round_asm!(majority, "ymm4", "ymm0", "ymm1", "ymm2", "ymm3", 56),
                round_asm!(majority, "ymm3", "ymm4", "ymm0", "ymm1", "ymm2", 57),
                round_asm!(majority, "ymm2", "ymm3", "ymm4", "ymm0", "ymm1", 58),
                round_asm!(majority, "ymm1", "ymm2", "ymm3", "ymm4", "ymm0", 59),
                round_asm!(parity, "ymm0", "ymm1", "ymm2", "ymm3", "ymm4", 60),
                round_asm!(parity, "ymm4", "ymm0", "ymm1", "ymm2", "ymm3", 61),
                round_asm!(parity, "ymm3", "ymm4", "ymm0", "ymm1", "ymm2", 62),
                round_asm!(parity, "ymm2", "ymm3", "ymm4", "ymm0", "ymm1", 63),
                round_asm!(parity, "ymm1", "ymm2", "ymm3", "ymm4", "ymm0", 64),
                round_asm!(parity, "ymm0", "ymm1", "ymm2", "ymm3", "ymm4", 65),
                round_asm!(parity, "ymm4", "ymm0", "ymm1", "ymm2", "ymm3", 66),
                round_asm!(parity, "ymm3", "ymm4", "ymm0", "ymm1", "ymm2", 67),
                round_asm!(parity, "ymm2", "ymm3", "ymm4", "ymm0", "ymm1", 68),
                round_asm!(parity, "ymm1", "ymm2", "ymm3", "ymm4", "ymm0", 69),
                round_asm!(parity, "ymm0", "ymm1", "ymm2", "ymm3", "ymm4", 70),
                round_asm!(parity, "ymm4", "ymm0", "ymm1", "ymm2", "ymm3", 71),
                round_asm!(parity, "ymm3", "ymm4", "ymm0", "ymm1", "ymm2", 72),
                round_asm!(parity, "ymm2", "ymm3", "ymm4", "ymm0", "ymm1", 73),
                round_asm!(parity, "ymm1", "ymm2", "ymm3", "ymm4", "ymm0", 74),
                round_asm!(parity, "ymm0", "ymm1", "ymm2", "ymm3", "ymm4", 75),
                round_asm!(parity, "ymm4", "ymm0", "ymm1", "ymm2", "ymm3", 76),
                round_asm!(parity, "ymm3", "ymm4", "ymm0", "ymm1", "ymm2", 77),
                round_asm!(parity, "ymm2", "ymm3", "ymm4", "ymm0", "ymm1", 78),
                round_asm!(parity, "ymm1", "ymm2", "ymm3", "ymm4", "ymm0", 79),
                in("r11") wk,
                inout("ymm0") a,
                inout("ymm1") b,
                inout("ymm2") c,
                inout("ymm3") d,
                inout("ymm4") e,
                out("ymm5") _,
                out("ymm6") _,
                options(nostack, readonly),
            );
            state[0] = _mm256_add_epi32(state[0], a);
            state[1] = _mm256_add_epi32(state[1], b);
            state[2] = _mm256_add_epi32(state[2], c);
            state[3] = _mm256_add_epi32(state[3], d);
            state[4] = _mm256_add_epi32(state[4], e);
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
            let mut state = [_mm256_setzero_si256(); 5];
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
                            let mut words = [[0u32; 8]; 5];
                            for (row, register) in words.iter_mut().zip(state) {
                                _mm256_storeu_si256(row.as_mut_ptr().cast(), register);
                            }
                            for (chunk, row) in
                                out[lane.job].as_chunks_mut::<4>().0.iter_mut().zip(words)
                            {
                                chunk.copy_from_slice(&row[index].to_be_bytes());
                            }
                        }
                        let Some(job) = queue.next() else {
                            blocks[index] = idle.as_ptr();
                            break;
                        };
                        // start the next message in this lane from the initial state
                        let mut words = [[0u32; 8]; 5];
                        for (row, register) in words.iter_mut().zip(state) {
                            _mm256_storeu_si256(row.as_mut_ptr().cast(), register);
                        }
                        for (row, initial) in words.iter_mut().zip(INITIAL) {
                            row[index] = initial;
                        }
                        for (register, row) in state.iter_mut().zip(words) {
                            *register = _mm256_loadu_si256(row.as_ptr().cast());
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
                compress(&mut state, &blocks);
            }
            out
        }
    }
}
