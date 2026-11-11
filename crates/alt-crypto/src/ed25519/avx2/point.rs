//! Points with their four coordinates in one [`F4`], so that the four
//! multiplications of an addition or a doubling run side by side.

use super::field::{A, B, C, D, F4, Order, order, pack};
use crate::ed25519::field::{D2, Fe};
use crate::ed25519::point::AffineNiels;

/// (X, Y, Z, T) with every limb at most one unit plus 2^18
#[derive(Clone, Copy)]
pub(super) struct Extended(F4);

/// (Y - X, Y + X, 2Z, 2dT), the form the second operand of an addition takes
#[derive(Clone, Copy)]
pub(super) struct Cached(F4);

const BABA: Order = order([1, 0, 1, 0]);
const ABAB: Order = order([0, 1, 0, 1]);
const AAAA: Order = order([0, 0, 0, 0]);
const BBBB: Order = order([1, 1, 1, 1]);
const CACA: Order = order([2, 0, 2, 0]);
const DBBD: Order = order([3, 1, 1, 3]);
const BBCC: Order = order([1, 1, 2, 2]);
const AADD: Order = order([0, 0, 3, 3]);
const ADCA: Order = order([0, 3, 2, 0]);
const CBDB: Order = order([2, 1, 3, 1]);
const ACDA: Order = order([0, 2, 3, 0]);
const DBCB: Order = order([3, 1, 2, 1]);

/// (1, 1, 2, 2d): what turns (Y - X, Y + X, Z, T) into a [`Cached`]
const SCALE: [[i32; 8]; 5] = pack([[1, 0, 0, 0, 0], [1, 0, 0, 0, 0], [2, 0, 0, 0, 0], D2.0]);

/// The packed lanes of a table of affine points, for [`Cached::from_packed`].
pub(super) const fn pack_table<const N: usize>(table: &[AffineNiels; N]) -> [[[i32; 8]; 5]; N] {
    let mut out = [[[0i32; 8]; 5]; N];
    let mut i = 0;
    while i < N {
        let [y_plus_x, y_minus_x, xy2d] = table[i].limbs();
        out[i] = pack([y_minus_x, y_plus_x, [2, 0, 0, 0, 0], xy2d]);
        i += 1;
    }
    out
}

impl Cached {
    #[target_feature(enable = "avx2")]
    #[inline]
    pub(super) fn from_packed(lanes: &[[i32; 8]; 5]) -> Cached {
        Cached(F4::from_packed(lanes))
    }
}

impl Extended {
    #[target_feature(enable = "avx2")]
    pub(super) fn identity() -> Extended {
        Extended(F4::new([Fe::ZERO, Fe::ONE, Fe::ONE, Fe::ZERO]))
    }

    /// from coordinates on 51-bit limbs below 2^51
    #[target_feature(enable = "avx2")]
    pub(super) fn new(coordinates: [Fe; 4]) -> Extended {
        Extended(F4::new(coordinates))
    }

    #[target_feature(enable = "avx2")]
    pub(super) fn coordinates(&self) -> [Fe; 4] {
        self.0.split()
    }

    /// (Y - X, Y + X, Z, T), at most three units
    #[target_feature(enable = "avx2")]
    #[inline]
    fn difference_and_sum(&self) -> F4 {
        let p = &self.0;
        // (Y + (2p - X), X + Y) in the first two lanes
        let mixed = p.swap_pairs().add(&p.blend::<A>(&p.negate()));
        p.blend::<{ A | B }>(&mixed)
    }

    #[target_feature(enable = "avx2")]
    pub(super) fn cached(&self) -> Cached {
        Cached(self.difference_and_sum().mul(&F4::from_packed(&SCALE)))
    }

    #[target_feature(enable = "avx2")]
    #[inline]
    pub(super) fn double(&self) -> Extended {
        let p = &self.0;
        // (X, Y, Z, X + Y)
        let sum = p.permute(&ABAB).add(&p.permute(&BABA));
        // (S1, S2, S3, -S4) = (X^2, Y^2, Z^2, -(X + Y)^2)
        let squares = p.blend::<D>(&sum).square(true);
        let zero = F4::from_packed(&[[0; 8]; 5]);
        let s1 = squares.permute(&AAAA);
        let s2 = squares.permute(&BBBB);
        // (S1 + S2, S1 - S2, S1 - S2 + 2 S3, S1 + S2 - S4), at most (2, 3, 5, 3) units
        let terms = zero
            .blend::<C>(&squares.add(&squares))
            .blend::<D>(&squares)
            .add(&s1)
            .add(&zero.blend::<{ A | D }>(&s2))
            .add(&zero.blend::<{ B | C }>(&s2.negate()));
        // the four products are the doubled point with every coordinate negated, which is the same point
        Extended(terms.permute(&CACA).mul(&terms.permute(&DBBD)))
    }

    /// (S1 - S0, S1 + S0, S2 - S3, S2 + S3) of the four products of an addition, at most three units
    #[target_feature(enable = "avx2")]
    #[inline]
    fn combine(&self, other: &F4) -> F4 {
        let products = self.difference_and_sum().mul(other);
        let outer = products.permute(&AADD);
        products
            .permute(&BBCC)
            .add(&outer.negate().blend::<{ B | D }>(&outer))
    }

    #[target_feature(enable = "avx2")]
    #[inline]
    pub(super) fn add(&self, other: &Cached) -> Extended {
        // (E, H, F, G) with X3 = E F, Y3 = G H, Z3 = F G, T3 = E H
        let terms = self.combine(&other.0);
        Extended(terms.permute(&ADCA).mul(&terms.permute(&CBDB)))
    }

    #[target_feature(enable = "avx2")]
    #[inline]
    pub(super) fn sub(&self, other: &Cached) -> Extended {
        // the negated point swaps Y - X with Y + X and negates 2dT, which swaps F and G
        let terms = self.combine(&other.0.swap_pairs().blend::<{ C | D }>(&other.0));
        Extended(terms.permute(&ACDA).mul(&terms.permute(&DBCB)))
    }
}
