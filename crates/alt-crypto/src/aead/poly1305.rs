//! Poly1305 one-time authenticator (RFC 8439 section 2.5).

/// A Poly1305 computation under a key that must never authenticate two messages.
///
/// The accumulator is kept below 2^130 + small in three 64-bit limbs and the
/// multiplier `r` in two.
pub struct Poly1305 {
    h: [u64; 3],
    r: [u64; 2],
    /// `r[1] * 5 / 4`, exact because clamping clears the low two bits of `r[1]`
    s1: u64,
    pad: [u64; 2],
    buffer: [u8; 16],
    filled: usize,
}

impl Poly1305 {
    pub fn new(key: &[u8; 32]) -> Self {
        let word = |at: usize| u64::from_le_bytes(*key[at..].first_chunk().expect("in range"));
        let r = [
            word(0) & 0x0fff_fffc_0fff_ffff,
            word(8) & 0x0fff_fffc_0fff_fffc,
        ];
        Self {
            h: [0; 3],
            r,
            s1: r[1] + (r[1] >> 2),
            pad: [word(16), word(24)],
            buffer: [0; 16],
            filled: 0,
        }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        if self.filled != 0 {
            let take = (16 - self.filled).min(data.len());
            self.buffer[self.filled..self.filled + take].copy_from_slice(&data[..take]);
            self.filled += take;
            data = &data[take..];
            if self.filled < 16 {
                return;
            }
            let block = self.buffer;
            self.blocks(&block, 1);
            self.filled = 0;
        }
        let whole = data.len() & !15;
        self.blocks(&data[..whole], 1);
        let rest = &data[whole..];
        self.buffer[..rest.len()].copy_from_slice(rest);
        self.filled = rest.len();
    }

    /// `update`, then zero bytes up to the next multiple of 16
    pub fn update_padded(&mut self, data: &[u8]) {
        self.update(data);
        if self.filled != 0 {
            self.buffer[self.filled..].fill(0);
            let block = self.buffer;
            self.blocks(&block, 1);
            self.filled = 0;
        }
    }

    pub fn finalize(mut self) -> [u8; 16] {
        if self.filled != 0 {
            // a short final block ends with a one bit in place of the implicit 2^128
            self.buffer[self.filled] = 1;
            self.buffer[self.filled + 1..].fill(0);
            let block = self.buffer;
            self.blocks(&block, 0);
        }
        let [h0, h1, h2] = self.h;
        // h + 5 carries into bit 130 exactly when h >= 2^130 - 5
        let t = h0 as u128 + 5;
        let g0 = t as u64;
        let t = h1 as u128 + (t >> 64);
        let g1 = t as u64;
        let g2 = h2.wrapping_add((t >> 64) as u64);
        let reduced = 0u64.wrapping_sub(g2 >> 2);
        let h0 = (h0 & !reduced) | (g0 & reduced);
        let h1 = (h1 & !reduced) | (g1 & reduced);
        let t = h0 as u128 + self.pad[0] as u128;
        let low = t as u64;
        let high = h1.wrapping_add(self.pad[1]).wrapping_add((t >> 64) as u64);
        let mut tag = [0u8; 16];
        tag[..8].copy_from_slice(&low.to_le_bytes());
        tag[8..].copy_from_slice(&high.to_le_bytes());
        tag
    }

    /// absorbs whole 16-byte blocks, each with `high` added at bit 128
    fn blocks(&mut self, data: &[u8], high: u64) {
        let [r0, r1] = self.r.map(u128::from);
        let s1 = self.s1 as u128;
        let [mut h0, mut h1, mut h2] = self.h;
        for block in data.as_chunks::<16>().0 {
            let (low, rest) = block.split_first_chunk::<8>().expect("16-byte block");
            let t = h0 as u128 + u64::from_le_bytes(*low) as u128;
            h0 = t as u64;
            let t = h1 as u128
                + (t >> 64)
                + u64::from_le_bytes(*rest.first_chunk().expect("16-byte block")) as u128;
            h1 = t as u64;
            h2 += (t >> 64) as u64 + high;

            // h2 stays below 8, so its products with r and s1 fit one limb
            let d0 = h0 as u128 * r0 + h1 as u128 * s1;
            let d1 = h0 as u128 * r1 + h1 as u128 * r0 + (h2 * self.s1) as u128 + (d0 >> 64);
            h2 = h2 * self.r[0] + (d1 >> 64) as u64;

            // fold everything above bit 130 back in, times five
            let fold = (h2 >> 2) + (h2 & !3);
            h2 &= 3;
            let t = d0 as u64 as u128 + fold as u128;
            h0 = t as u64;
            let t = d1 as u64 as u128 + (t >> 64);
            h1 = t as u64;
            h2 += (t >> 64) as u64;
        }
        self.h = [h0, h1, h2];
    }
}
