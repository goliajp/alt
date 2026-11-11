//! SHA-512 (FIPS 180-4).

/// streaming SHA-512
#[derive(Clone)]
pub struct Sha512 {
    state: [u64; 8],
    bytes: u128,
    buffer: [u8; 128],
    filled: usize,
}

/// the initial hash value
pub const INITIAL: [u64; 8] = [
    0x6a09_e667_f3bc_c908,
    0xbb67_ae85_84ca_a73b,
    0x3c6e_f372_fe94_f82b,
    0xa54f_f53a_5f1d_36f1,
    0x510e_527f_ade6_82d1,
    0x9b05_688c_2b3e_6c1f,
    0x1f83_d9ab_fb41_bd6b,
    0x5be0_cd19_137e_2179,
];

#[rustfmt::skip]
static K: [u64; 80] = [
    0x428a2f98d728ae22, 0x7137449123ef65cd, 0xb5c0fbcfec4d3b2f, 0xe9b5dba58189dbbc,
    0x3956c25bf348b538, 0x59f111f1b605d019, 0x923f82a4af194f9b, 0xab1c5ed5da6d8118,
    0xd807aa98a3030242, 0x12835b0145706fbe, 0x243185be4ee4b28c, 0x550c7dc3d5ffb4e2,
    0x72be5d74f27b896f, 0x80deb1fe3b1696b1, 0x9bdc06a725c71235, 0xc19bf174cf692694,
    0xe49b69c19ef14ad2, 0xefbe4786384f25e3, 0x0fc19dc68b8cd5b5, 0x240ca1cc77ac9c65,
    0x2de92c6f592b0275, 0x4a7484aa6ea6e483, 0x5cb0a9dcbd41fbd4, 0x76f988da831153b5,
    0x983e5152ee66dfab, 0xa831c66d2db43210, 0xb00327c898fb213f, 0xbf597fc7beef0ee4,
    0xc6e00bf33da88fc2, 0xd5a79147930aa725, 0x06ca6351e003826f, 0x142929670a0e6e70,
    0x27b70a8546d22ffc, 0x2e1b21385c26c926, 0x4d2c6dfc5ac42aed, 0x53380d139d95b3df,
    0x650a73548baf63de, 0x766a0abb3c77b2a8, 0x81c2c92e47edaee6, 0x92722c851482353b,
    0xa2bfe8a14cf10364, 0xa81a664bbc423001, 0xc24b8b70d0f89791, 0xc76c51a30654be30,
    0xd192e819d6ef5218, 0xd69906245565a910, 0xf40e35855771202a, 0x106aa07032bbd1b8,
    0x19a4c116b8d2d0c8, 0x1e376c085141ab53, 0x2748774cdf8eeb99, 0x34b0bcb5e19b48a8,
    0x391c0cb3c5c95a63, 0x4ed8aa4ae3418acb, 0x5b9cca4f7763e373, 0x682e6ff3d6b2b8a3,
    0x748f82ee5defb2fc, 0x78a5636f43172f60, 0x84c87814a1f0ab72, 0x8cc702081a6439ec,
    0x90befffa23631e28, 0xa4506cebde82bde9, 0xbef9a3f7b2c67915, 0xc67178f2e372532b,
    0xca273eceea26619c, 0xd186b8c721c0c207, 0xeada7dd6cde0eb1e, 0xf57d4f7fee6ed178,
    0x06f067aa72176fba, 0x0a637dc5a2c898a6, 0x113f9804bef90dae, 0x1b710b35131c471b,
    0x28db77f523047d84, 0x32caab7b40c72493, 0x3c9ebe0a15c9bebc, 0x431d67c49c100d4c,
    0x4cc5d4becb3e42b6, 0x597f299cfc657e2a, 0x5fcb6fab3ad6faec, 0x6c44198c4a475817,
];

impl Default for Sha512 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha512 {
    pub fn new() -> Self {
        Self {
            state: INITIAL,
            bytes: 0,
            buffer: [0; 128],
            filled: 0,
        }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.bytes = self.bytes.wrapping_add(data.len() as u128);
        if self.filled != 0 {
            let take = (128 - self.filled).min(data.len());
            self.buffer[self.filled..self.filled + take].copy_from_slice(&data[..take]);
            self.filled += take;
            data = &data[take..];
            if self.filled < 128 {
                return;
            }
            let block = self.buffer;
            compress(&mut self.state, &block);
            self.filled = 0;
        }
        let whole = data.len() & !127;
        if whole != 0 {
            compress(&mut self.state, &data[..whole]);
        }
        let rest = &data[whole..];
        self.buffer[..rest.len()].copy_from_slice(rest);
        self.filled = rest.len();
    }

    pub fn finalize(mut self) -> [u8; 64] {
        let bits = self.bytes.wrapping_mul(8);
        let mut tail = [0u8; 256];
        tail[..self.filled].copy_from_slice(&self.buffer[..self.filled]);
        tail[self.filled] = 0x80;
        let total = if self.filled < 112 { 128 } else { 256 };
        tail[total - 16..total].copy_from_slice(&bits.to_be_bytes());
        compress(&mut self.state, &tail[..total]);
        let mut out = [0u8; 64];
        for (chunk, word) in out.as_chunks_mut::<8>().0.iter_mut().zip(self.state) {
            *chunk = word.to_be_bytes();
        }
        out
    }
}

/// SHA-512 of one buffer
pub fn sha512(data: &[u8]) -> [u8; 64] {
    let mut hasher = Sha512::new();
    hasher.update(data);
    hasher.finalize()
}

type Compress = unsafe fn(&mut [u64; 8], &[u8]);

/// the fastest implementation this CPU supports, and its name
fn select() -> (&'static str, Compress) {
    #[cfg(target_arch = "aarch64")]
    if std::arch::is_aarch64_feature_detected!("sha3") {
        return ("sha512 instructions", arm::compress);
    }
    #[cfg(target_arch = "x86_64")]
    {
        use std::arch::is_x86_feature_detected as detected;
        if detected!("avx2") && detected!("bmi1") && detected!("bmi2") {
            return ("avx2", x86::compress);
        }
    }
    ("portable", portable)
}

/// which implementation runs on this CPU
pub fn implementation() -> &'static str {
    select().0
}

/// folds whole 128-byte blocks into `state`
fn compress(state: &mut [u64; 8], blocks: &[u8]) {
    debug_assert!(blocks.len().is_multiple_of(128));
    // an implementation is only selected after its CPU features were detected
    unsafe { select().1(state, blocks) }
}

/// the schedule word for round `t`, kept in a rolling window of sixteen
#[inline(always)]
fn word(w: &mut [u64; 16], t: usize) -> u64 {
    if t >= 16 {
        let w15 = w[(t + 1) & 15];
        let w2 = w[(t + 14) & 15];
        let s0 = w15.rotate_right(1) ^ w15.rotate_right(8) ^ (w15 >> 7);
        let s1 = w2.rotate_right(19) ^ w2.rotate_right(61) ^ (w2 >> 6);
        w[t & 15] = w[t & 15]
            .wrapping_add(s0)
            .wrapping_add(w[(t + 9) & 15])
            .wrapping_add(s1);
    }
    w[t & 15]
}

macro_rules! round {
    ($a:ident, $b:ident, $c:ident, $d:ident, $e:ident, $f:ident, $g:ident, $h:ident, $t:expr, $w:ident) => {
        let t1 = $h
            .wrapping_add($e.rotate_right(14) ^ $e.rotate_right(18) ^ $e.rotate_right(41))
            .wrapping_add($g ^ ($e & ($f ^ $g)))
            .wrapping_add(K[$t])
            .wrapping_add(word(&mut $w, $t));
        let t2 = ($a.rotate_right(28) ^ $a.rotate_right(34) ^ $a.rotate_right(39))
            .wrapping_add(($a & $b) | ($c & ($a | $b)));
        $d = $d.wrapping_add(t1);
        $h = t1.wrapping_add(t2);
    };
}

/// the reference implementation every accelerated one is compared with
pub fn portable(state: &mut [u64; 8], blocks: &[u8]) {
    for block in blocks.as_chunks::<128>().0 {
        let mut w = [0u64; 16];
        for (slot, bytes) in w.iter_mut().zip(block.as_chunks::<8>().0) {
            *slot = u64::from_be_bytes(*bytes);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
        for t in (0..80).step_by(8) {
            round!(a, b, c, d, e, f, g, h, t, w);
            round!(h, a, b, c, d, e, f, g, t + 1, w);
            round!(g, h, a, b, c, d, e, f, t + 2, w);
            round!(f, g, h, a, b, c, d, e, t + 3, w);
            round!(e, f, g, h, a, b, c, d, t + 4, w);
            round!(d, e, f, g, h, a, b, c, t + 5, w);
            round!(c, d, e, f, g, h, a, b, t + 6, w);
            round!(b, c, d, e, f, g, h, a, t + 7, w);
        }
        for (slot, value) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *slot = slot.wrapping_add(value);
        }
    }
}

#[cfg(target_arch = "aarch64")]
mod arm;
#[cfg(target_arch = "x86_64")]
mod x86;
