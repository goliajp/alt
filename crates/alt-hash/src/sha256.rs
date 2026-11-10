//! SHA-256 (FIPS 180-4).

/// streaming SHA-256
#[derive(Clone)]
pub struct Sha256 {
    state: [u32; 8],
    bytes: u64,
    buffer: [u8; 64],
    filled: usize,
}

const INITIAL: [u32; 8] = [
    0x6a09_e667,
    0xbb67_ae85,
    0x3c6e_f372,
    0xa54f_f53a,
    0x510e_527f,
    0x9b05_688c,
    0x1f83_d9ab,
    0x5be0_cd19,
];
static K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha256 {
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

    pub fn finalize(mut self) -> [u8; 32] {
        let bits = self.bytes.wrapping_mul(8);
        let mut tail = [0u8; 128];
        tail[..self.filled].copy_from_slice(&self.buffer[..self.filled]);
        tail[self.filled] = 0x80;
        let total = if self.filled < 56 { 64 } else { 128 };
        tail[total - 8..total].copy_from_slice(&bits.to_be_bytes());
        compress(&mut self.state, &tail[..total]);
        let mut out = [0u8; 32];
        for (chunk, word) in out.as_chunks_mut::<4>().0.iter_mut().zip(self.state) {
            *chunk = word.to_be_bytes();
        }
        out
    }
}

/// SHA-256 of one buffer
pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize()
}

/// folds whole 64-byte blocks into `state`
fn compress(state: &mut [u32; 8], blocks: &[u8]) {
    debug_assert!(blocks.len().is_multiple_of(64));
    #[cfg(target_arch = "aarch64")]
    if std::arch::is_aarch64_feature_detected!("sha2") {
        // the feature was just detected on this CPU
        unsafe { arm::compress(state, blocks) };
        return;
    }
    portable(state, blocks);
}

/// the schedule word for round `t`, kept in a rolling window of sixteen
#[inline(always)]
fn word(w: &mut [u32; 16], t: usize) -> u32 {
    if t >= 16 {
        let w15 = w[(t + 1) & 15];
        let w2 = w[(t + 14) & 15];
        let s0 = w15.rotate_right(7) ^ w15.rotate_right(18) ^ (w15 >> 3);
        let s1 = w2.rotate_right(17) ^ w2.rotate_right(19) ^ (w2 >> 10);
        w[t & 15] = w[t & 15]
            .wrapping_add(s0)
            .wrapping_add(w[(t + 9) & 15])
            .wrapping_add(s1);
    }
    w[t & 15]
}

/// One round with the working variables in the given roles. Only `d` and `h`
/// change, so eight consecutive rounds rotate the roles instead of moving data.
macro_rules! round {
    ($a:ident, $b:ident, $c:ident, $d:ident, $e:ident, $f:ident, $g:ident, $h:ident, $t:expr, $w:ident) => {
        let t1 = $h
            .wrapping_add($e.rotate_right(6) ^ $e.rotate_right(11) ^ $e.rotate_right(25))
            .wrapping_add($g ^ ($e & ($f ^ $g)))
            .wrapping_add(K[$t])
            .wrapping_add(word(&mut $w, $t));
        let t2 = ($a.rotate_right(2) ^ $a.rotate_right(13) ^ $a.rotate_right(22))
            .wrapping_add(($a & $b) | ($c & ($a | $b)));
        $d = $d.wrapping_add(t1);
        $h = t1.wrapping_add(t2);
    };
}

/// the reference implementation every accelerated one is compared with
pub fn portable(state: &mut [u32; 8], blocks: &[u8]) {
    for block in blocks.as_chunks::<64>().0 {
        let mut w = [0u32; 16];
        for (slot, bytes) in w.iter_mut().zip(block.as_chunks::<4>().0) {
            *slot = u32::from_be_bytes(*bytes);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
        round!(a, b, c, d, e, f, g, h, 0, w);
        round!(h, a, b, c, d, e, f, g, 1, w);
        round!(g, h, a, b, c, d, e, f, 2, w);
        round!(f, g, h, a, b, c, d, e, 3, w);
        round!(e, f, g, h, a, b, c, d, 4, w);
        round!(d, e, f, g, h, a, b, c, 5, w);
        round!(c, d, e, f, g, h, a, b, 6, w);
        round!(b, c, d, e, f, g, h, a, 7, w);
        round!(a, b, c, d, e, f, g, h, 8, w);
        round!(h, a, b, c, d, e, f, g, 9, w);
        round!(g, h, a, b, c, d, e, f, 10, w);
        round!(f, g, h, a, b, c, d, e, 11, w);
        round!(e, f, g, h, a, b, c, d, 12, w);
        round!(d, e, f, g, h, a, b, c, 13, w);
        round!(c, d, e, f, g, h, a, b, 14, w);
        round!(b, c, d, e, f, g, h, a, 15, w);
        round!(a, b, c, d, e, f, g, h, 16, w);
        round!(h, a, b, c, d, e, f, g, 17, w);
        round!(g, h, a, b, c, d, e, f, 18, w);
        round!(f, g, h, a, b, c, d, e, 19, w);
        round!(e, f, g, h, a, b, c, d, 20, w);
        round!(d, e, f, g, h, a, b, c, 21, w);
        round!(c, d, e, f, g, h, a, b, 22, w);
        round!(b, c, d, e, f, g, h, a, 23, w);
        round!(a, b, c, d, e, f, g, h, 24, w);
        round!(h, a, b, c, d, e, f, g, 25, w);
        round!(g, h, a, b, c, d, e, f, 26, w);
        round!(f, g, h, a, b, c, d, e, 27, w);
        round!(e, f, g, h, a, b, c, d, 28, w);
        round!(d, e, f, g, h, a, b, c, 29, w);
        round!(c, d, e, f, g, h, a, b, 30, w);
        round!(b, c, d, e, f, g, h, a, 31, w);
        round!(a, b, c, d, e, f, g, h, 32, w);
        round!(h, a, b, c, d, e, f, g, 33, w);
        round!(g, h, a, b, c, d, e, f, 34, w);
        round!(f, g, h, a, b, c, d, e, 35, w);
        round!(e, f, g, h, a, b, c, d, 36, w);
        round!(d, e, f, g, h, a, b, c, 37, w);
        round!(c, d, e, f, g, h, a, b, 38, w);
        round!(b, c, d, e, f, g, h, a, 39, w);
        round!(a, b, c, d, e, f, g, h, 40, w);
        round!(h, a, b, c, d, e, f, g, 41, w);
        round!(g, h, a, b, c, d, e, f, 42, w);
        round!(f, g, h, a, b, c, d, e, 43, w);
        round!(e, f, g, h, a, b, c, d, 44, w);
        round!(d, e, f, g, h, a, b, c, 45, w);
        round!(c, d, e, f, g, h, a, b, 46, w);
        round!(b, c, d, e, f, g, h, a, 47, w);
        round!(a, b, c, d, e, f, g, h, 48, w);
        round!(h, a, b, c, d, e, f, g, 49, w);
        round!(g, h, a, b, c, d, e, f, 50, w);
        round!(f, g, h, a, b, c, d, e, 51, w);
        round!(e, f, g, h, a, b, c, d, 52, w);
        round!(d, e, f, g, h, a, b, c, 53, w);
        round!(c, d, e, f, g, h, a, b, 54, w);
        round!(b, c, d, e, f, g, h, a, 55, w);
        round!(a, b, c, d, e, f, g, h, 56, w);
        round!(h, a, b, c, d, e, f, g, 57, w);
        round!(g, h, a, b, c, d, e, f, 58, w);
        round!(f, g, h, a, b, c, d, e, 59, w);
        round!(e, f, g, h, a, b, c, d, 60, w);
        round!(d, e, f, g, h, a, b, c, 61, w);
        round!(c, d, e, f, g, h, a, b, 62, w);
        round!(b, c, d, e, f, g, h, a, 63, w);
        for (slot, value) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *slot = slot.wrapping_add(value);
        }
    }
}

#[cfg(target_arch = "aarch64")]
mod arm;
