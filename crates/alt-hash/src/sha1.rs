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

    /// SHA-1 with the ARMv8 cryptography extension: four rounds per instruction.
    ///
    /// Written as assembly so that the rotated `e` never leaves the vector registers
    /// and each group's message-plus-constant is ready two groups ahead.
    /// v0: a b c d, s1 / s2: e (alternating), v4-v7: schedule, v16-v19: constants,
    /// v20 / v21: message plus constant, v22 / v23: state at block entry.
    #[target_feature(enable = "sha2")]
    pub(super) unsafe fn compress(state: &mut [u32; 5], blocks: &[u8]) {
        let count = blocks.len() / 64;
        if count == 0 {
            return;
        }
        unsafe {
            std::arch::asm!(
                "ld1 {{v0.4s}}, [x0]",
                "ldr s1, [x0, #16]",
                "ld1r {{v16.4s}}, [x3], #4",
                "ld1r {{v17.4s}}, [x3], #4",
                "ld1r {{v18.4s}}, [x3], #4",
                "ld1r {{v19.4s}}, [x3]",
                "2:",
                "ld1 {{v4.16b, v5.16b, v6.16b, v7.16b}}, [x1], #64",
                "rev32 v4.16b, v4.16b",
                "rev32 v5.16b, v5.16b",
                "rev32 v6.16b, v6.16b",
                "rev32 v7.16b, v7.16b",
                "mov v22.16b, v0.16b",
                "mov v23.16b, v1.16b",
                "add v20.4s, v4.4s, v16.4s",
                "add v21.4s, v5.4s, v16.4s",
                "sha1h s2, s0",
                "sha1c q0, s1, v20.4s",
                "add v20.4s, v6.4s, v16.4s",
                "sha1su0 v4.4s, v5.4s, v6.4s",
                "sha1h s1, s0",
                "sha1c q0, s2, v21.4s",
                "add v21.4s, v7.4s, v16.4s",
                "sha1su1 v4.4s, v7.4s",
                "sha1su0 v5.4s, v6.4s, v7.4s",
                "sha1h s2, s0",
                "sha1c q0, s1, v20.4s",
                "add v20.4s, v4.4s, v16.4s",
                "sha1su1 v5.4s, v4.4s",
                "sha1su0 v6.4s, v7.4s, v4.4s",
                "sha1h s1, s0",
                "sha1c q0, s2, v21.4s",
                "add v21.4s, v5.4s, v17.4s",
                "sha1su1 v6.4s, v5.4s",
                "sha1su0 v7.4s, v4.4s, v5.4s",
                "sha1h s2, s0",
                "sha1c q0, s1, v20.4s",
                "add v20.4s, v6.4s, v17.4s",
                "sha1su1 v7.4s, v6.4s",
                "sha1su0 v4.4s, v5.4s, v6.4s",
                "sha1h s1, s0",
                "sha1p q0, s2, v21.4s",
                "add v21.4s, v7.4s, v17.4s",
                "sha1su1 v4.4s, v7.4s",
                "sha1su0 v5.4s, v6.4s, v7.4s",
                "sha1h s2, s0",
                "sha1p q0, s1, v20.4s",
                "add v20.4s, v4.4s, v17.4s",
                "sha1su1 v5.4s, v4.4s",
                "sha1su0 v6.4s, v7.4s, v4.4s",
                "sha1h s1, s0",
                "sha1p q0, s2, v21.4s",
                "add v21.4s, v5.4s, v17.4s",
                "sha1su1 v6.4s, v5.4s",
                "sha1su0 v7.4s, v4.4s, v5.4s",
                "sha1h s2, s0",
                "sha1p q0, s1, v20.4s",
                "add v20.4s, v6.4s, v18.4s",
                "sha1su1 v7.4s, v6.4s",
                "sha1su0 v4.4s, v5.4s, v6.4s",
                "sha1h s1, s0",
                "sha1p q0, s2, v21.4s",
                "add v21.4s, v7.4s, v18.4s",
                "sha1su1 v4.4s, v7.4s",
                "sha1su0 v5.4s, v6.4s, v7.4s",
                "sha1h s2, s0",
                "sha1m q0, s1, v20.4s",
                "add v20.4s, v4.4s, v18.4s",
                "sha1su1 v5.4s, v4.4s",
                "sha1su0 v6.4s, v7.4s, v4.4s",
                "sha1h s1, s0",
                "sha1m q0, s2, v21.4s",
                "add v21.4s, v5.4s, v18.4s",
                "sha1su1 v6.4s, v5.4s",
                "sha1su0 v7.4s, v4.4s, v5.4s",
                "sha1h s2, s0",
                "sha1m q0, s1, v20.4s",
                "add v20.4s, v6.4s, v18.4s",
                "sha1su1 v7.4s, v6.4s",
                "sha1su0 v4.4s, v5.4s, v6.4s",
                "sha1h s1, s0",
                "sha1m q0, s2, v21.4s",
                "add v21.4s, v7.4s, v19.4s",
                "sha1su1 v4.4s, v7.4s",
                "sha1su0 v5.4s, v6.4s, v7.4s",
                "sha1h s2, s0",
                "sha1m q0, s1, v20.4s",
                "add v20.4s, v4.4s, v19.4s",
                "sha1su1 v5.4s, v4.4s",
                "sha1su0 v6.4s, v7.4s, v4.4s",
                "sha1h s1, s0",
                "sha1p q0, s2, v21.4s",
                "add v21.4s, v5.4s, v19.4s",
                "sha1su1 v6.4s, v5.4s",
                "sha1su0 v7.4s, v4.4s, v5.4s",
                "sha1h s2, s0",
                "sha1p q0, s1, v20.4s",
                "add v20.4s, v6.4s, v19.4s",
                "sha1su1 v7.4s, v6.4s",
                "sha1h s1, s0",
                "sha1p q0, s2, v21.4s",
                "add v21.4s, v7.4s, v19.4s",
                "sha1h s2, s0",
                "sha1p q0, s1, v20.4s",
                "sha1h s1, s0",
                "sha1p q0, s2, v21.4s",
                "add v0.4s, v0.4s, v22.4s",
                "add v1.4s, v1.4s, v23.4s",
                "subs x2, x2, #1",
                "b.ne 2b",
                "st1 {{v0.4s}}, [x0]",
                "str s1, [x0, #16]",
                in("x0") state.as_mut_ptr(),
                inout("x1") blocks.as_ptr() => _,
                inout("x2") count => _,
                inout("x3") K.as_ptr() => _,
                out("v0") _, out("v1") _, out("v2") _, out("v4") _, out("v5") _, out("v6") _, out("v7") _,
                out("v16") _, out("v17") _, out("v18") _, out("v19") _,
                out("v20") _, out("v21") _, out("v22") _, out("v23") _,
                options(nostack),
            );
        }
    }
}
