//! Four elements of GF(2^255 - 19) side by side, each on ten limbs of
//! alternately 26 and 25 bits.
//!
//! Vector `i` holds limbs `2i` and `2i + 1` of the elements A, B, C, D as
//! the 32-bit lanes (A even, B even, A odd, B odd, C even, D even, C odd,
//! D odd), so that one unpack turns it into 64-bit lanes ready for
//! `vpmuludq`.
//!
//! Bounds are counted in units of 2^26 for even limbs and 2^25 for odd
//! ones. `mul` and `square` return limbs of at most one unit plus 2^18;
//! what they accept is stated on each.

use super::super::field::Fe;
use std::arch::x86_64::*;

/// 32-bit lanes of each element, for `blend`
pub(super) const A: i32 = 0b0000_0101;
pub(super) const B: i32 = 0b0000_1010;
pub(super) const C: i32 = 0b0101_0000;
pub(super) const D: i32 = 0b1010_0000;

/// a rearrangement of the four elements, as `vpermd` indices
pub(super) type Order = [i32; 8];

/// the order in which output element `k` is input element `from[k]` (0 for A to 3 for D)
pub(super) const fn order(from: [usize; 4]) -> Order {
    const FIRST: [usize; 4] = [0, 1, 4, 5];
    let mut out = [0i32; 8];
    let mut k = 0;
    while k < 4 {
        out[FIRST[k]] = FIRST[from[k]] as i32;
        out[FIRST[k] + 2] = FIRST[from[k]] as i32 + 2;
        k += 1;
    }
    out
}

/// the 32-bit lanes of the five vectors holding four elements given as 51-bit limbs below 2^51
pub(super) const fn pack(elements: [[u64; 5]; 4]) -> [[i32; 8]; 5] {
    const FIRST: [usize; 4] = [0, 1, 4, 5];
    let mut out = [[0i32; 8]; 5];
    let mut i = 0;
    while i < 5 {
        let mut e = 0;
        while e < 4 {
            out[i][FIRST[e]] = (elements[e][i] & ((1 << 26) - 1)) as i32;
            out[i][FIRST[e] + 2] = (elements[e][i] >> 26) as i32;
            e += 1;
        }
        i += 1;
    }
    out
}

#[derive(Clone, Copy)]
pub(super) struct F4(pub(super) [__m256i; 5]);

#[target_feature(enable = "avx2")]
#[inline]
fn vector(lanes: [i32; 8]) -> __m256i {
    let [l0, l1, l2, l3, l4, l5, l6, l7] = lanes;
    _mm256_setr_epi32(l0, l1, l2, l3, l4, l5, l6, l7)
}

/// 2p, limb by limb, for negation without borrows
const TWICE_P: [[i32; 8]; 5] = {
    let even = (1 << 27) - 2;
    let odd = (1 << 26) - 2;
    let mut out = [[even, even, odd, odd, even, even, odd, odd]; 5];
    out[0][0] -= 36;
    out[0][1] -= 36;
    out[0][4] -= 36;
    out[0][5] -= 36;
    out
};

impl F4 {
    #[target_feature(enable = "avx2")]
    #[inline]
    pub(super) fn from_packed(lanes: &[[i32; 8]; 5]) -> F4 {
        F4(lanes.map(|lanes| vector(lanes)))
    }

    /// four elements whose 51-bit limbs are below 2^51
    #[target_feature(enable = "avx2")]
    pub(super) fn new(elements: [Fe; 4]) -> F4 {
        Self::from_packed(&pack(elements.map(|element| element.0)))
    }

    /// the four elements on 51-bit limbs, each limb below 2^52
    #[target_feature(enable = "avx2")]
    pub(super) fn split(&self) -> [Fe; 4] {
        let mut out = [Fe::ZERO; 4];
        for (i, vector) in self.0.iter().enumerate() {
            let lane = |index: i32| -> u64 {
                let value = match index {
                    0 => _mm256_extract_epi32::<0>(*vector),
                    1 => _mm256_extract_epi32::<1>(*vector),
                    2 => _mm256_extract_epi32::<2>(*vector),
                    3 => _mm256_extract_epi32::<3>(*vector),
                    4 => _mm256_extract_epi32::<4>(*vector),
                    5 => _mm256_extract_epi32::<5>(*vector),
                    6 => _mm256_extract_epi32::<6>(*vector),
                    _ => _mm256_extract_epi32::<7>(*vector),
                };
                u64::from(value as u32)
            };
            for (element, first) in out.iter_mut().zip([0, 1, 4, 5]) {
                element.0[i] = lane(first) + (lane(first + 2) << 26);
            }
        }
        out
    }

    #[target_feature(enable = "avx2")]
    #[inline]
    pub(super) fn add(&self, other: &F4) -> F4 {
        let mut out = self.0;
        for (limbs, theirs) in out.iter_mut().zip(other.0) {
            *limbs = _mm256_add_epi32(*limbs, theirs);
        }
        F4(out)
    }

    /// 2p - self, without carrying: `self` at most one unit plus 2^18, result at most two units
    #[target_feature(enable = "avx2")]
    #[inline]
    pub(super) fn negate(&self) -> F4 {
        let mut out = self.0;
        for (limbs, twice_p) in out.iter_mut().zip(TWICE_P) {
            *limbs = _mm256_sub_epi32(vector(twice_p), *limbs);
        }
        F4(out)
    }

    /// the elements of `other` in the lanes named by `LANES`, those of `self` elsewhere
    #[target_feature(enable = "avx2")]
    #[inline]
    pub(super) fn blend<const LANES: i32>(&self, other: &F4) -> F4 {
        let mut out = self.0;
        for (limbs, theirs) in out.iter_mut().zip(other.0) {
            *limbs = _mm256_blend_epi32::<LANES>(*limbs, theirs);
        }
        F4(out)
    }

    #[target_feature(enable = "avx2")]
    #[inline]
    pub(super) fn permute(&self, order: &Order) -> F4 {
        let indices = vector(*order);
        F4(self
            .0
            .map(|limbs| _mm256_permutevar8x32_epi32(limbs, indices)))
    }

    /// (B, A, D, C): each pair of neighbours trades places
    #[target_feature(enable = "avx2")]
    #[inline]
    pub(super) fn swap_pairs(&self) -> F4 {
        F4(self
            .0
            .map(|limbs| _mm256_shuffle_epi32::<0b10_11_00_01>(limbs)))
    }

    /// the ten limbs as 64-bit lanes, one vector per limb
    #[target_feature(enable = "avx2")]
    #[inline]
    fn unpack(&self) -> [__m256i; 10] {
        let zero = _mm256_setzero_si256();
        let mut out = [zero; 10];
        for (i, limbs) in self.0.iter().enumerate() {
            out[2 * i] = _mm256_unpacklo_epi32(*limbs, zero);
            out[2 * i + 1] = _mm256_unpackhi_epi32(*limbs, zero);
        }
        out
    }

    /// The products of corresponding elements. `self` at most eight units, `other` at most three.
    #[target_feature(enable = "avx2")]
    #[inline]
    pub(super) fn mul(&self, other: &F4) -> F4 {
        let x = self.unpack();
        let y = other.unpack();
        let nineteen = _mm256_set1_epi64x(19);
        let x2 = x.map(|limb| _mm256_add_epi64(limb, limb));
        let y19 = y.map(|limb| _mm256_mul_epu32(limb, nineteen));
        // limb i times limb j lands on limb i + j, which past limb 9 wraps with
        // a factor 19; two odd limbs meet half a bit above their sum's
        // position, hence the doubled x
        #[rustfmt::skip]
        let z = [
            sum([m(x[0], y[0]), m(x2[1], y19[9]), m(x[2], y19[8]), m(x2[3], y19[7]), m(x[4], y19[6]),
                 m(x2[5], y19[5]), m(x[6], y19[4]), m(x2[7], y19[3]), m(x[8], y19[2]), m(x2[9], y19[1])]),
            sum([m(x[0], y[1]), m(x[1], y[0]), m(x[2], y19[9]), m(x[3], y19[8]), m(x[4], y19[7]),
                 m(x[5], y19[6]), m(x[6], y19[5]), m(x[7], y19[4]), m(x[8], y19[3]), m(x[9], y19[2])]),
            sum([m(x[0], y[2]), m(x2[1], y[1]), m(x[2], y[0]), m(x2[3], y19[9]), m(x[4], y19[8]),
                 m(x2[5], y19[7]), m(x[6], y19[6]), m(x2[7], y19[5]), m(x[8], y19[4]), m(x2[9], y19[3])]),
            sum([m(x[0], y[3]), m(x[1], y[2]), m(x[2], y[1]), m(x[3], y[0]), m(x[4], y19[9]),
                 m(x[5], y19[8]), m(x[6], y19[7]), m(x[7], y19[6]), m(x[8], y19[5]), m(x[9], y19[4])]),
            sum([m(x[0], y[4]), m(x2[1], y[3]), m(x[2], y[2]), m(x2[3], y[1]), m(x[4], y[0]),
                 m(x2[5], y19[9]), m(x[6], y19[8]), m(x2[7], y19[7]), m(x[8], y19[6]), m(x2[9], y19[5])]),
            sum([m(x[0], y[5]), m(x[1], y[4]), m(x[2], y[3]), m(x[3], y[2]), m(x[4], y[1]),
                 m(x[5], y[0]), m(x[6], y19[9]), m(x[7], y19[8]), m(x[8], y19[7]), m(x[9], y19[6])]),
            sum([m(x[0], y[6]), m(x2[1], y[5]), m(x[2], y[4]), m(x2[3], y[3]), m(x[4], y[2]),
                 m(x2[5], y[1]), m(x[6], y[0]), m(x2[7], y19[9]), m(x[8], y19[8]), m(x2[9], y19[7])]),
            sum([m(x[0], y[7]), m(x[1], y[6]), m(x[2], y[5]), m(x[3], y[4]), m(x[4], y[3]),
                 m(x[5], y[2]), m(x[6], y[1]), m(x[7], y[0]), m(x[8], y19[9]), m(x[9], y19[8])]),
            sum([m(x[0], y[8]), m(x2[1], y[7]), m(x[2], y[6]), m(x2[3], y[5]), m(x[4], y[4]),
                 m(x2[5], y[3]), m(x[6], y[2]), m(x2[7], y[1]), m(x[8], y[0]), m(x2[9], y19[9])]),
            sum([m(x[0], y[9]), m(x[1], y[8]), m(x[2], y[7]), m(x[3], y[6]), m(x[4], y[5]),
                 m(x[5], y[4]), m(x[6], y[3]), m(x[7], y[2]), m(x[8], y[1]), m(x[9], y[0])]),
        ];
        carry(z)
    }

    /// The squares of the elements, element D negated when `negate_d` is
    /// set. `self` at most two units.
    #[target_feature(enable = "avx2")]
    #[inline]
    pub(super) fn square(&self, negate_d: bool) -> F4 {
        let x = self.unpack();
        let nineteen = _mm256_set1_epi64x(19);
        let x2 = x.map(|limb| _mm256_add_epi64(limb, limb));
        let x4 = x2.map(|limb| _mm256_add_epi64(limb, limb));
        let x19 = x.map(|limb| _mm256_mul_epu32(limb, nineteen));
        // as in `mul`, with each product of two different limbs taken once and doubled
        #[rustfmt::skip]
        let mut z = [
            sum([m(x[0], x[0]), m(x4[1], x19[9]), m(x2[2], x19[8]), m(x4[3], x19[7]), m(x2[4], x19[6]),
                 m(x2[5], x19[5])]),
            sum([m(x2[0], x[1]), m(x2[2], x19[9]), m(x2[3], x19[8]), m(x2[4], x19[7]), m(x2[5], x19[6])]),
            sum([m(x2[0], x[2]), m(x2[1], x[1]), m(x4[3], x19[9]), m(x2[4], x19[8]), m(x4[5], x19[7]),
                 m(x[6], x19[6])]),
            sum([m(x2[0], x[3]), m(x2[1], x[2]), m(x2[4], x19[9]), m(x2[5], x19[8]), m(x2[6], x19[7])]),
            sum([m(x2[0], x[4]), m(x4[1], x[3]), m(x[2], x[2]), m(x4[5], x19[9]), m(x2[6], x19[8]),
                 m(x2[7], x19[7])]),
            sum([m(x2[0], x[5]), m(x2[1], x[4]), m(x2[2], x[3]), m(x2[6], x19[9]), m(x2[7], x19[8])]),
            sum([m(x2[0], x[6]), m(x4[1], x[5]), m(x2[2], x[4]), m(x2[3], x[3]), m(x4[7], x19[9]),
                 m(x[8], x19[8])]),
            sum([m(x2[0], x[7]), m(x2[1], x[6]), m(x2[2], x[5]), m(x2[3], x[4]), m(x2[8], x19[9])]),
            sum([m(x2[0], x[8]), m(x4[1], x[7]), m(x2[2], x[6]), m(x4[3], x[5]), m(x[4], x[4]),
                 m(x2[9], x19[9])]),
            sum([m(x2[0], x[9]), m(x2[1], x[8]), m(x2[2], x[7]), m(x2[3], x[6]), m(x2[4], x[5])]),
        ];
        if negate_d {
            // p * 2^37 exceeds any limb sum here (below 2^61), so the difference stays positive
            let limb = |bits: u32, less: i64| ((1i64 << bits) - less) << 37;
            for (i, sum) in z.iter_mut().enumerate() {
                let multiple = match i {
                    0 => limb(26, 19),
                    i if i % 2 == 0 => limb(26, 1),
                    _ => limb(25, 1),
                };
                let negated = _mm256_sub_epi64(_mm256_set1_epi64x(multiple), *sum);
                *sum = _mm256_blend_epi32::<0b1100_0000>(*sum, negated);
            }
        }
        carry(z)
    }
}

#[target_feature(enable = "avx2")]
#[inline]
fn m(a: __m256i, b: __m256i) -> __m256i {
    _mm256_mul_epu32(a, b)
}

#[target_feature(enable = "avx2")]
#[inline]
fn sum<const N: usize>(terms: [__m256i; N]) -> __m256i {
    let mut total = terms[0];
    for term in &terms[1..] {
        total = _mm256_add_epi64(total, *term);
    }
    total
}

/// Carries ten 64-bit limb sums (each below 2^63.5) into packed limbs: even
/// limbs below 2^26, odd limbs below 2^25 + 2^18.
#[target_feature(enable = "avx2")]
#[inline]
fn carry(mut z: [__m256i; 10]) -> F4 {
    let mask_26 = _mm256_set1_epi64x((1 << 26) - 1);
    let mask_25 = _mm256_set1_epi64x((1 << 25) - 1);
    // two chains, 0-1-2-3-4 and 4-5-6-7-8-9, advance together
    for i in [0, 4, 1, 5, 2, 6, 3, 7, 4, 8] {
        let out = if i % 2 == 0 {
            let out = _mm256_srli_epi64::<26>(z[i]);
            z[i] = _mm256_and_si256(z[i], mask_26);
            out
        } else {
            let out = _mm256_srli_epi64::<25>(z[i]);
            z[i] = _mm256_and_si256(z[i], mask_25);
            out
        };
        z[i + 1] = _mm256_add_epi64(z[i + 1], out);
    }
    // what leaves limb 9 re-enters at limb 0 times 19; it can exceed 32 bits,
    // so its upper part goes to limb 1, which is 2^26 times limb 0
    let nineteen = _mm256_set1_epi64x(19);
    let out = _mm256_srli_epi64::<25>(z[9]);
    z[9] = _mm256_and_si256(z[9], mask_25);
    let low = _mm256_mul_epu32(_mm256_and_si256(out, mask_26), nineteen);
    let high = _mm256_mul_epu32(_mm256_srli_epi64::<26>(out), nineteen);
    z[0] = _mm256_add_epi64(z[0], low);
    z[1] = _mm256_add_epi64(z[1], high);
    let out = _mm256_srli_epi64::<26>(z[0]);
    z[0] = _mm256_and_si256(z[0], mask_26);
    z[1] = _mm256_add_epi64(z[1], out);

    let mut packed = [_mm256_setzero_si256(); 5];
    for (i, limbs) in packed.iter_mut().enumerate() {
        // (even, odd) per 64-bit lane becomes (even, even, odd, odd) per 128-bit lane
        let pairs = _mm256_or_si256(z[2 * i], _mm256_slli_epi64::<32>(z[2 * i + 1]));
        *limbs = _mm256_shuffle_epi32::<0b11_01_10_00>(pairs);
    }
    F4(packed)
}
