//! BLAKE2b (RFC 7693), unkeyed, with a digest length of 1 to 64 bytes.

#[cfg(target_arch = "x86_64")]
mod avx2;

/// the initialization vector, shared with SHA-512
pub const IV: [u64; 8] = super::sha512::INITIAL;

const SIGMA: [[u8; 16]; 12] = [
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
    [14, 10, 4, 8, 9, 15, 13, 6, 1, 12, 0, 2, 11, 7, 5, 3],
    [11, 8, 12, 0, 5, 2, 15, 13, 10, 14, 3, 6, 7, 1, 9, 4],
    [7, 9, 3, 1, 13, 12, 11, 14, 2, 6, 5, 10, 4, 0, 15, 8],
    [9, 0, 5, 7, 2, 4, 10, 15, 14, 1, 11, 12, 6, 8, 3, 13],
    [2, 12, 6, 10, 0, 11, 8, 3, 4, 13, 7, 5, 15, 14, 1, 9],
    [12, 5, 1, 15, 14, 13, 4, 10, 0, 7, 6, 3, 9, 2, 8, 11],
    [13, 11, 7, 14, 12, 1, 3, 9, 5, 0, 15, 4, 8, 6, 2, 10],
    [6, 15, 14, 9, 11, 3, 0, 8, 12, 2, 13, 7, 1, 4, 10, 5],
    [10, 2, 8, 4, 7, 6, 1, 5, 15, 11, 9, 14, 3, 12, 13, 0],
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
    [14, 10, 4, 8, 9, 15, 13, 6, 1, 12, 0, 2, 11, 7, 5, 3],
];

/// streaming BLAKE2b
#[derive(Clone)]
pub struct Blake2b {
    state: [u64; 8],
    bytes: u128,
    buffer: [u8; 128],
    filled: usize,
    out_len: usize,
}

impl Blake2b {
    /// A hasher producing `out_len` bytes.
    ///
    /// # Panics
    /// When `out_len` is not in `1..=64`.
    pub fn new(out_len: usize) -> Self {
        assert!((1..=64).contains(&out_len), "blake2b digest length");
        let mut state = IV;
        state[0] ^= 0x0101_0000 ^ out_len as u64;
        Self {
            state,
            bytes: 0,
            buffer: [0; 128],
            filled: 0,
            out_len,
        }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        // the last block is compressed by `finalize`, so a full buffer waits for more input
        if self.filled + data.len() <= 128 {
            self.buffer[self.filled..self.filled + data.len()].copy_from_slice(data);
            self.filled += data.len();
            return;
        }
        if self.filled != 0 {
            let take = 128 - self.filled;
            self.buffer[self.filled..].copy_from_slice(&data[..take]);
            data = &data[take..];
            self.bytes += 128;
            compress(&mut self.state, &self.buffer, self.bytes, false);
            self.filled = 0;
        }
        while data.len() > 128 {
            let (block, rest) = data
                .split_first_chunk::<128>()
                .expect("more than one block");
            self.bytes += 128;
            compress(&mut self.state, block, self.bytes, false);
            data = rest;
        }
        self.buffer[..data.len()].copy_from_slice(data);
        self.filled = data.len();
    }

    /// Writes the digest to `out`.
    ///
    /// # Panics
    /// When `out` is not exactly the digest length given to [`Blake2b::new`].
    pub fn finalize(mut self, out: &mut [u8]) {
        assert_eq!(out.len(), self.out_len, "blake2b digest length");
        self.buffer[self.filled..].fill(0);
        self.bytes += self.filled as u128;
        compress(&mut self.state, &self.buffer, self.bytes, true);
        let (words, tail) = out.as_chunks_mut::<8>();
        for (chunk, word) in words.iter_mut().zip(self.state) {
            *chunk = word.to_le_bytes();
        }
        let partial = self.state[words.len() % 8].to_le_bytes();
        tail.copy_from_slice(&partial[..tail.len()]);
    }
}

macro_rules! g {
    ($a:ident, $b:ident, $c:ident, $d:ident, $x:expr, $y:expr) => {
        $a = $a.wrapping_add($b).wrapping_add($x);
        $d = ($d ^ $a).rotate_right(32);
        $c = $c.wrapping_add($d);
        $b = ($b ^ $c).rotate_right(24);
        $a = $a.wrapping_add($b).wrapping_add($y);
        $d = ($d ^ $a).rotate_right(16);
        $c = $c.wrapping_add($d);
        $b = ($b ^ $c).rotate_right(63);
    };
}

type Compress = unsafe fn(&mut [u64; 8], &[u8; 128], u128, bool);

/// the fastest implementation this CPU supports, and its name
fn select() -> (&'static str, Option<Compress>) {
    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("avx2") {
        return ("avx2", Some(avx2::compress));
    }
    ("portable", None)
}

/// which implementation runs on this CPU
pub fn implementation() -> &'static str {
    select().0
}

/// The compression function F: folds one block into `state`, with `bytes`
/// the input length up to and including this block.
pub fn compress(state: &mut [u64; 8], block: &[u8; 128], bytes: u128, last: bool) {
    match select().1 {
        // an implementation is only selected after its CPU features were detected
        Some(accelerated) => unsafe { accelerated(state, block, bytes, last) },
        None => portable(state, block, bytes, last),
    }
}

/// The reference implementation every accelerated one is compared with.
pub fn portable(state: &mut [u64; 8], block: &[u8; 128], bytes: u128, last: bool) {
    let mut m = [0u64; 16];
    for (slot, word) in m.iter_mut().zip(block.as_chunks::<8>().0) {
        *slot = u64::from_le_bytes(*word);
    }
    let [
        mut v0,
        mut v1,
        mut v2,
        mut v3,
        mut v4,
        mut v5,
        mut v6,
        mut v7,
    ] = *state;
    let [
        mut v8,
        mut v9,
        mut v10,
        mut v11,
        mut v12,
        mut v13,
        mut v14,
        mut v15,
    ] = IV;
    v12 ^= bytes as u64;
    v13 ^= (bytes >> 64) as u64;
    if last {
        v14 = !v14;
    }
    // the message word at position `i` of round `r`; the indices are constants once inlined
    macro_rules! round {
        ($r:literal) => {
            let s = &SIGMA[$r];
            g!(v0, v4, v8, v12, m[s[0] as usize], m[s[1] as usize]);
            g!(v1, v5, v9, v13, m[s[2] as usize], m[s[3] as usize]);
            g!(v2, v6, v10, v14, m[s[4] as usize], m[s[5] as usize]);
            g!(v3, v7, v11, v15, m[s[6] as usize], m[s[7] as usize]);
            g!(v0, v5, v10, v15, m[s[8] as usize], m[s[9] as usize]);
            g!(v1, v6, v11, v12, m[s[10] as usize], m[s[11] as usize]);
            g!(v2, v7, v8, v13, m[s[12] as usize], m[s[13] as usize]);
            g!(v3, v4, v9, v14, m[s[14] as usize], m[s[15] as usize]);
        };
    }
    round!(0);
    round!(1);
    round!(2);
    round!(3);
    round!(4);
    round!(5);
    round!(6);
    round!(7);
    round!(8);
    round!(9);
    round!(10);
    round!(11);
    let mixed = [
        v0 ^ v8,
        v1 ^ v9,
        v2 ^ v10,
        v3 ^ v11,
        v4 ^ v12,
        v5 ^ v13,
        v6 ^ v14,
        v7 ^ v15,
    ];
    for (slot, word) in state.iter_mut().zip(mixed) {
        *slot ^= word;
    }
}
