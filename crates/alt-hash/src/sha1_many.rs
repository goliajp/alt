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

    #[inline(always)]
    unsafe fn word(w: &mut [__m256i; 16], t: usize) -> __m256i {
        unsafe { if t < 16 { w[t] } else { schedule(w, t) } }
    }

    /// One round with the working variables in the given roles; only `e` and `b` change.
    macro_rules! round {
        ($a:ident, $b:ident, $c:ident, $d:ident, $e:ident, $f:expr, $k:expr, $word:expr) => {
            $e = _mm256_add_epi32(
                _mm256_add_epi32($e, rotate::<5, 27>($a)),
                _mm256_add_epi32(_mm256_add_epi32($f, $k), $word),
            );
            $b = rotate::<30, 2>($b);
        };
    }
    macro_rules! five {
        ($w:ident, $t:expr, $k:expr, $f:ident, $a:ident, $b:ident, $c:ident, $d:ident, $e:ident) => {
            round!($a, $b, $c, $d, $e, $f!($b, $c, $d), $k, word(&mut $w, $t));
            round!(
                $e,
                $a,
                $b,
                $c,
                $d,
                $f!($a, $b, $c),
                $k,
                word(&mut $w, $t + 1)
            );
            round!(
                $d,
                $e,
                $a,
                $b,
                $c,
                $f!($e, $a, $b),
                $k,
                word(&mut $w, $t + 2)
            );
            round!(
                $c,
                $d,
                $e,
                $a,
                $b,
                $f!($d, $e, $a),
                $k,
                word(&mut $w, $t + 3)
            );
            round!(
                $b,
                $c,
                $d,
                $e,
                $a,
                $f!($c, $d, $e),
                $k,
                word(&mut $w, $t + 4)
            );
        };
    }
    macro_rules! choose {
        ($b:expr, $c:expr, $d:expr) => {
            _mm256_xor_si256($d, _mm256_and_si256($b, _mm256_xor_si256($c, $d)))
        };
    }
    macro_rules! parity {
        ($b:expr, $c:expr, $d:expr) => {
            _mm256_xor_si256(_mm256_xor_si256($b, $c), $d)
        };
    }
    macro_rules! majority {
        ($b:expr, $c:expr, $d:expr) => {
            _mm256_or_si256(
                _mm256_and_si256($b, $c),
                _mm256_and_si256($d, _mm256_or_si256($b, $c)),
            )
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
            let [mut a, mut b, mut c, mut d, mut e] = *state;
            five!(w, 0, k[0], choose, a, b, c, d, e);
            five!(w, 5, k[0], choose, a, b, c, d, e);
            five!(w, 10, k[0], choose, a, b, c, d, e);
            five!(w, 15, k[0], choose, a, b, c, d, e);
            five!(w, 20, k[1], parity, a, b, c, d, e);
            five!(w, 25, k[1], parity, a, b, c, d, e);
            five!(w, 30, k[1], parity, a, b, c, d, e);
            five!(w, 35, k[1], parity, a, b, c, d, e);
            five!(w, 40, k[2], majority, a, b, c, d, e);
            five!(w, 45, k[2], majority, a, b, c, d, e);
            five!(w, 50, k[2], majority, a, b, c, d, e);
            five!(w, 55, k[2], majority, a, b, c, d, e);
            five!(w, 60, k[3], parity, a, b, c, d, e);
            five!(w, 65, k[3], parity, a, b, c, d, e);
            five!(w, 70, k[3], parity, a, b, c, d, e);
            five!(w, 75, k[3], parity, a, b, c, d, e);
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
            let mut order: Vec<usize> = (0..messages.len()).collect();
            let length = |job: usize| {
                messages[job]
                    .iter()
                    .map(|part| part.len() as u64)
                    .sum::<u64>()
            };
            order.sort_by_key(|job| std::cmp::Reverse(length(*job)));
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
                            bytes: length(job),
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
