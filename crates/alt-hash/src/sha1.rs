//! SHA-1 (FIPS 180-4).

/// streaming SHA-1
#[derive(Clone)]
pub struct Sha1 {
    state: [u32; 5],
    bytes: u64,
    buffer: [u8; 64],
    filled: usize,
}

const INITIAL: [u32; 5] = [
    0x6745_2301,
    0xefcd_ab89,
    0x98ba_dcfe,
    0x1032_5476,
    0xc3d2_e1f0,
];
const K: [u32; 4] = [0x5a82_7999, 0x6ed9_eba1, 0x8f1b_bcdc, 0xca62_c1d6];

impl Default for Sha1 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha1 {
    pub fn new() -> Self {
        Self {
            state: INITIAL,
            bytes: 0,
            buffer: [0; 64],
            filled: 0,
        }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.bytes = self.bytes.wrapping_add(data.len() as u64);
        if self.filled != 0 {
            let take = (64 - self.filled).min(data.len());
            self.buffer[self.filled..self.filled + take].copy_from_slice(&data[..take]);
            self.filled += take;
            data = &data[take..];
            if self.filled < 64 {
                return;
            }
            let block = self.buffer;
            compress(&mut self.state, &block);
            self.filled = 0;
        }
        let whole = data.len() & !63;
        if whole != 0 {
            compress(&mut self.state, &data[..whole]);
        }
        let rest = &data[whole..];
        self.buffer[..rest.len()].copy_from_slice(rest);
        self.filled = rest.len();
    }

    pub fn finalize(mut self) -> [u8; 20] {
        let bits = self.bytes.wrapping_mul(8);
        let mut tail = [0u8; 128];
        tail[..self.filled].copy_from_slice(&self.buffer[..self.filled]);
        tail[self.filled] = 0x80;
        let total = if self.filled < 56 { 64 } else { 128 };
        tail[total - 8..total].copy_from_slice(&bits.to_be_bytes());
        compress(&mut self.state, &tail[..total]);
        let mut out = [0u8; 20];
        for (chunk, word) in out.as_chunks_mut::<4>().0.iter_mut().zip(self.state) {
            chunk.copy_from_slice(&word.to_be_bytes());
        }
        out
    }
}

/// SHA-1 of one buffer
pub fn sha1(data: &[u8]) -> [u8; 20] {
    let mut hasher = Sha1::new();
    hasher.update(data);
    hasher.finalize()
}

/// folds whole 64-byte blocks into `state`
fn compress(state: &mut [u32; 5], blocks: &[u8]) {
    debug_assert!(blocks.len().is_multiple_of(64));
    #[cfg(target_arch = "aarch64")]
    if std::arch::is_aarch64_feature_detected!("sha2") {
        // the feature was just detected on this CPU
        unsafe { arm::compress(state, blocks) };
        return;
    }
    portable(state, blocks);
}

/// the next schedule word, kept in a rolling window of sixteen
#[inline(always)]
fn schedule(w: &mut [u32; 16], t: usize) -> u32 {
    let word = (w[(t + 13) & 15] ^ w[(t + 8) & 15] ^ w[(t + 2) & 15] ^ w[t & 15]).rotate_left(1);
    w[t & 15] = word;
    word
}

/// One round with the working variables in the given roles. Only `e` and `b`
/// change, so five consecutive rounds rotate the roles instead of moving data.
macro_rules! round {
    ($a:ident, $b:ident, $c:ident, $d:ident, $e:ident, $f:expr, $k:expr, $word:expr) => {
        $e = $e
            .wrapping_add($a.rotate_left(5))
            .wrapping_add($f)
            .wrapping_add($k)
            .wrapping_add($word);
        $b = $b.rotate_left(30);
    };
}

/// five rounds starting at schedule position `$t`
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
        $d ^ ($b & ($c ^ $d))
    };
}
macro_rules! parity {
    ($b:expr, $c:expr, $d:expr) => {
        $b ^ $c ^ $d
    };
}
macro_rules! majority {
    ($b:expr, $c:expr, $d:expr) => {
        ($b & $c) | ($d & ($b | $c))
    };
}

/// the schedule word for round `t`: the message itself for the first sixteen
#[inline(always)]
fn word(w: &mut [u32; 16], t: usize) -> u32 {
    if t < 16 { w[t] } else { schedule(w, t) }
}

/// the reference implementation every accelerated one is compared with
pub fn portable(state: &mut [u32; 5], blocks: &[u8]) {
    for block in blocks.as_chunks::<64>().0 {
        let mut w = [0u32; 16];
        for (slot, bytes) in w.iter_mut().zip(block.as_chunks::<4>().0) {
            *slot = u32::from_be_bytes(*bytes);
        }
        let [mut a, mut b, mut c, mut d, mut e] = *state;
        five!(w, 0, K[0], choose, a, b, c, d, e);
        five!(w, 5, K[0], choose, a, b, c, d, e);
        five!(w, 10, K[0], choose, a, b, c, d, e);
        five!(w, 15, K[0], choose, a, b, c, d, e);
        five!(w, 20, K[1], parity, a, b, c, d, e);
        five!(w, 25, K[1], parity, a, b, c, d, e);
        five!(w, 30, K[1], parity, a, b, c, d, e);
        five!(w, 35, K[1], parity, a, b, c, d, e);
        five!(w, 40, K[2], majority, a, b, c, d, e);
        five!(w, 45, K[2], majority, a, b, c, d, e);
        five!(w, 50, K[2], majority, a, b, c, d, e);
        five!(w, 55, K[2], majority, a, b, c, d, e);
        five!(w, 60, K[3], parity, a, b, c, d, e);
        five!(w, 65, K[3], parity, a, b, c, d, e);
        five!(w, 70, K[3], parity, a, b, c, d, e);
        five!(w, 75, K[3], parity, a, b, c, d, e);
        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
        state[4] = state[4].wrapping_add(e);
    }
}

#[cfg(target_arch = "aarch64")]
mod arm {
    use super::K;
    use std::arch::aarch64::*;

    /// SHA-1 with the ARMv8 cryptography extension: four rounds per instruction
    #[target_feature(enable = "sha2")]
    pub(super) unsafe fn compress(state: &mut [u32; 5], blocks: &[u8]) {
        unsafe {
            let mut abcd = vld1q_u32(state.as_ptr());
            let mut e = state[4];
            for block in blocks.as_chunks::<64>().0 {
                let (saved_abcd, saved_e) = (abcd, e);
                let mut m = [
                    vreinterpretq_u32_u8(vrev32q_u8(vld1q_u8(block.as_ptr()))),
                    vreinterpretq_u32_u8(vrev32q_u8(vld1q_u8(block.as_ptr().add(16)))),
                    vreinterpretq_u32_u8(vrev32q_u8(vld1q_u8(block.as_ptr().add(32)))),
                    vreinterpretq_u32_u8(vrev32q_u8(vld1q_u8(block.as_ptr().add(48)))),
                ];
                for group in 0..20 {
                    let slot = group % 4;
                    let input = vaddq_u32(m[slot], vdupq_n_u32(K[group / 5]));
                    let next_e = vsha1h_u32(vgetq_lane_u32(abcd, 0));
                    abcd = match group / 5 {
                        0 => vsha1cq_u32(abcd, e, input),
                        2 => vsha1mq_u32(abcd, e, input),
                        _ => vsha1pq_u32(abcd, e, input),
                    };
                    e = next_e;
                    if group < 16 {
                        // the four schedule words used four groups from now
                        m[slot] = vsha1su1q_u32(
                            vsha1su0q_u32(m[slot], m[(slot + 1) % 4], m[(slot + 2) % 4]),
                            m[(slot + 3) % 4],
                        );
                    }
                }
                abcd = vaddq_u32(abcd, saved_abcd);
                e = e.wrapping_add(saved_e);
            }
            vst1q_u32(state.as_mut_ptr(), abcd);
            state[4] = e;
        }
    }
}
