//! Arithmetic modulo the group order L = 2^252 + 27742317777372353535851937790883648493
//! on five 52-bit limbs, with Montgomery reduction (R = 2^260).

use crate::wipe::wipe;

const MASK: u64 = (1 << 52) - 1;

const L: [u64; 5] = [
    0x0002_631a_5cf5_d3ed,
    0x000d_ea2f_79cd_6581,
    0x0000_0000_0014_def9,
    0,
    0x0000_1000_0000_0000,
];
/// -1 / L modulo 2^52
const L_FACTOR: u64 = 0x0005_1da3_1254_7e1b;
/// 2^260 modulo L
const R: Scalar = Scalar([
    0x000f_48bd_6721_e6ed,
    0x0003_bab5_ac67_e45a,
    0x000f_ffff_eb35_e51b,
    0x000f_ffff_ffff_ffff,
    0x0000_0fff_ffff_ffff,
]);
/// 2^520 modulo L
const RR: Scalar = Scalar([
    0x0009_d265_e952_d13b,
    0x000d_63c7_15be_a69f,
    0x0005_be65_cb68_7604,
    0x0003_dcee_c73d_217f,
    0x0000_0941_1b7c_309a,
]);

const L_BYTES: [u8; 32] = [
    237, 211, 245, 92, 26, 99, 18, 88, 214, 156, 247, 162, 222, 249, 222, 20, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 16,
];

/// A residue modulo L, fully reduced unless a function says otherwise.
#[derive(Clone, Copy)]
pub(super) struct Scalar([u64; 5]);

/// Whether 32 little-endian bytes encode a number below L. Not constant
/// time: used on the public S half of a signature.
pub(super) fn is_canonical(bytes: &[u8; 32]) -> bool {
    bytes.iter().rev().lt(L_BYTES.iter().rev())
}

impl Scalar {
    /// the 520-bit little-endian number in two 260-bit halves, unreduced
    fn unpack(bytes: &[u8; 64]) -> (Scalar, Scalar) {
        let mut words = [0u64; 9];
        for (word, chunk) in words.iter_mut().zip(bytes.as_chunks::<8>().0) {
            *word = u64::from_le_bytes(*chunk);
        }
        let low = Scalar([
            words[0] & MASK,
            ((words[0] >> 52) | (words[1] << 12)) & MASK,
            ((words[1] >> 40) | (words[2] << 24)) & MASK,
            ((words[2] >> 28) | (words[3] << 36)) & MASK,
            ((words[3] >> 16) | (words[4] << 48)) & MASK,
        ]);
        let high = Scalar([
            (words[4] >> 4) & MASK,
            ((words[4] >> 56) | (words[5] << 8)) & MASK,
            ((words[5] >> 44) | (words[6] << 20)) & MASK,
            ((words[6] >> 32) | (words[7] << 32)) & MASK,
            words[7] >> 20,
        ]);
        (low, high)
    }

    /// a 512-bit little-endian number reduced modulo L
    pub(super) fn from_wide(bytes: &[u8; 64]) -> Scalar {
        let (low, high) = Self::unpack(bytes);
        // low * R / R + high * R^2 / R = low + high * 2^260
        let low = montgomery_reduce(product(&low, &R));
        let high = montgomery_reduce(product(&high, &RR));
        low.add(&high)
    }

    /// a 256-bit little-endian number reduced modulo L
    pub(super) fn from_bytes_reduced(bytes: &[u8; 32]) -> Scalar {
        let mut wide = [0u8; 64];
        wide[..32].copy_from_slice(bytes);
        let out = Self::from_wide(&wide);
        wipe(&mut wide);
        out
    }

    pub(super) fn to_bytes(self) -> [u8; 32] {
        let l = &self.0;
        let words = [
            l[0] | (l[1] << 52),
            (l[1] >> 12) | (l[2] << 40),
            (l[2] >> 24) | (l[3] << 28),
            (l[3] >> 36) | (l[4] << 16),
        ];
        let mut out = [0u8; 32];
        for (chunk, word) in out.as_chunks_mut::<8>().0.iter_mut().zip(words) {
            *chunk = word.to_le_bytes();
        }
        out
    }

    pub(super) fn add(&self, rhs: &Scalar) -> Scalar {
        let mut sum = self.0;
        let mut carry = 0;
        for (limb, other) in sum.iter_mut().zip(rhs.0) {
            carry = *limb + other + (carry >> 52);
            *limb = carry & MASK;
        }
        Scalar(sum).sub_order()
    }

    /// subtracts L when the value is at least L; the value must be below 2L
    fn sub_order(&self) -> Scalar {
        let mut difference = self.0;
        let mut borrow = 0u64;
        for (limb, order) in difference.iter_mut().zip(L) {
            borrow = limb.wrapping_sub(order + (borrow >> 63));
            *limb = borrow & MASK;
        }
        // a final borrow means the value was below L: add L back under a mask
        let underflow = 0u64.wrapping_sub(borrow >> 63);
        let mut carry = 0;
        for (limb, order) in difference.iter_mut().zip(L) {
            carry = *limb + (order & underflow) + (carry >> 52);
            *limb = carry & MASK;
        }
        Scalar(difference)
    }

    pub(super) fn mul(&self, rhs: &Scalar) -> Scalar {
        let ab = montgomery_reduce(product(self, rhs));
        montgomery_reduce(product(&ab, &RR))
    }

    pub(super) fn wipe(&mut self) {
        wipe(&mut self.0);
    }
}

/// the nine-limb product, with a spare limb for the reduction's carries
fn product(a: &Scalar, b: &Scalar) -> [u128; 10] {
    let mut out = [0u128; 10];
    for i in 0..5 {
        for j in 0..5 {
            out[i + j] += a.0[i] as u128 * b.0[j] as u128;
        }
    }
    out
}

/// t / R modulo L, for t below L * R
fn montgomery_reduce(mut t: [u128; 10]) -> Scalar {
    for i in 0..5 {
        // the multiple of L that clears the low limb
        let n = (t[i] as u64).wrapping_mul(L_FACTOR) & MASK;
        for j in 0..5 {
            t[i + j] += n as u128 * L[j] as u128;
        }
        t[i + 1] += t[i] >> 52;
    }
    let mut out = [0u64; 5];
    let mut carry = 0;
    for (slot, high) in out.iter_mut().zip(&t[5..]) {
        let limb = high + carry;
        *slot = limb as u64 & MASK;
        carry = limb >> 52;
    }
    Scalar(out).sub_order()
}

/// Signed radix-16 digits in [-8, 8] of a number below 2^255, least
/// significant first.
pub(super) fn radix_16(bytes: &[u8; 32]) -> [i8; 64] {
    let mut digits = [0i8; 64];
    for (i, byte) in bytes.iter().enumerate() {
        digits[2 * i] = (byte & 15) as i8;
        digits[2 * i + 1] = (byte >> 4) as i8;
    }
    for i in 0..63 {
        let carry = (digits[i] + 8) >> 4;
        digits[i] -= carry << 4;
        digits[i + 1] += carry;
    }
    digits
}

/// Width-`w` non-adjacent form of a number below 2^255: every nonzero
/// digit is odd, below 2^(w-1) in magnitude, and followed by at least
/// `w - 1` zeros. Not constant time.
pub(super) fn non_adjacent_form(bytes: &[u8; 32], w: usize) -> [i8; 256] {
    let mut words = [0u64; 5];
    for (word, chunk) in words.iter_mut().zip(bytes.as_chunks::<8>().0) {
        *word = u64::from_le_bytes(*chunk);
    }
    let width = 1u64 << w;
    let mut naf = [0i8; 256];
    let mut position = 0;
    let mut carry = 0;
    while position < 256 {
        let (word, bit) = (position / 64, position % 64);
        let bits = if bit < 64 - w {
            words[word] >> bit
        } else {
            (words[word] >> bit) | (words[word + 1] << (64 - bit))
        };
        let window = carry + (bits & (width - 1));
        if window & 1 == 0 {
            position += 1;
            continue;
        }
        if window < width / 2 {
            carry = 0;
            naf[position] = window as i8;
        } else {
            carry = 1;
            naf[position] = (window as i64 - width as i64) as i8;
        }
        position += w;
    }
    naf
}
