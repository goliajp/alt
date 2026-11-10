//! LSB-first bit output.

pub struct BitWriter {
    out: Vec<u8>,
    acc: u64,
    filled: u32,
}

impl BitWriter {
    pub fn new(out: Vec<u8>) -> Self {
        Self {
            out,
            acc: 0,
            filled: 0,
        }
    }

    /// `value` must fit in `len` bits, `len <= 16`
    pub fn put(&mut self, value: u32, len: u32) {
        self.acc |= u64::from(value) << self.filled;
        self.filled += len;
        if self.filled >= 32 {
            self.out.extend_from_slice(&(self.acc as u32).to_le_bytes());
            self.acc >>= 32;
            self.filled -= 32;
        }
    }

    /// Pads with zero bits up to the next byte boundary.
    pub fn align(&mut self) {
        let bytes = self.filled.div_ceil(8) as usize;
        self.out.extend_from_slice(&self.acc.to_le_bytes()[..bytes]);
        self.acc = 0;
        self.filled = 0;
    }

    /// only valid on a byte boundary
    pub fn put_bytes(&mut self, bytes: &[u8]) {
        debug_assert_eq!(self.filled, 0);
        self.out.extend_from_slice(bytes);
    }

    pub fn finish(mut self) -> Vec<u8> {
        self.align();
        self.out
    }
}
