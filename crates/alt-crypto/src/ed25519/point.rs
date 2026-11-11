//! The twisted Edwards curve -x^2 + y^2 = 1 + d x^2 y^2 in extended coordinates.

use super::field::{D, D2, Fe};
use super::scalar;
use super::tables::{BASE_ODD, BASE_WINDOWS};
use crate::wipe::wipe;
use std::hint::black_box;

/// (X : Y : Z : T) with x = X/Z, y = Y/Z, xy = T/Z
#[derive(Clone, Copy)]
pub(super) struct Point {
    x: Fe,
    y: Fe,
    z: Fe,
    t: Fe,
}

/// (X : Y : Z), enough for doubling
#[derive(Clone, Copy)]
struct Projective {
    x: Fe,
    y: Fe,
    z: Fe,
}

/// ((X : Z), (Y : T)) with x = X/Z, y = Y/T: the result of an addition
/// or doubling before the multiplications that bring it to another form
struct Completed {
    x: Fe,
    y: Fe,
    z: Fe,
    t: Fe,
}

/// an affine point prepared for addition: (y + x, y - x, 2dxy)
#[derive(Clone, Copy)]
pub(super) struct AffineNiels {
    y_plus_x: Fe,
    y_minus_x: Fe,
    xy2d: Fe,
}

/// a projective point prepared for addition: (Y + X, Y - X, Z, 2dT)
#[derive(Clone, Copy)]
struct ProjectiveNiels {
    y_plus_x: Fe,
    y_minus_x: Fe,
    z: Fe,
    t2d: Fe,
}

impl AffineNiels {
    const IDENTITY: AffineNiels = AffineNiels {
        y_plus_x: Fe::ONE,
        y_minus_x: Fe::ONE,
        xy2d: Fe::ZERO,
    };

    pub(super) const fn from_limbs(
        y_plus_x: [u64; 5],
        y_minus_x: [u64; 5],
        xy2d: [u64; 5],
    ) -> Self {
        Self {
            y_plus_x: Fe(y_plus_x),
            y_minus_x: Fe(y_minus_x),
            xy2d: Fe(xy2d),
        }
    }

    /// `digit * P` for `digit` in [-8, 8], where `window[j]` is `(j + 1) * P`.
    /// Every entry is read and the choice is made with masks, so neither the
    /// memory accessed nor the instructions executed depend on `digit`.
    fn select(window: &[AffineNiels; 8], digit: i8) -> AffineNiels {
        let sign = digit >> 7;
        let magnitude = ((digit ^ sign) - sign) as u8;
        let mut out = AffineNiels::IDENTITY;
        for (multiple, entry) in (1u8..).zip(window) {
            let different = u64::from(magnitude ^ multiple);
            let hit = black_box((different.wrapping_sub(1) >> 63).wrapping_neg());
            out.y_plus_x = out.y_plus_x.select(&entry.y_plus_x, hit);
            out.y_minus_x = out.y_minus_x.select(&entry.y_minus_x, hit);
            out.xy2d = out.xy2d.select(&entry.xy2d, hit);
        }
        let negative = black_box(sign as i64 as u64);
        AffineNiels {
            y_plus_x: out.y_plus_x.select(&out.y_minus_x, negative),
            y_minus_x: out.y_minus_x.select(&out.y_plus_x, negative),
            xy2d: out.xy2d.select(&out.xy2d.neg(), negative),
        }
    }
}

impl Projective {
    const IDENTITY: Projective = Projective {
        x: Fe::ZERO,
        y: Fe::ONE,
        z: Fe::ONE,
    };

    fn double(&self) -> Completed {
        let xx = self.x.square();
        let yy = self.y.square();
        let zz2 = self.z.square2();
        let sum_squared = self.x.add(&self.y).square();
        let yy_plus_xx = yy.add(&xx);
        let yy_minus_xx = yy.sub(&xx);
        Completed {
            x: sum_squared.sub(&yy_plus_xx),
            y: yy_plus_xx,
            z: yy_minus_xx,
            t: zz2.sub(&yy_minus_xx),
        }
    }
}

impl Completed {
    fn projective(&self) -> Projective {
        Projective {
            x: self.x.mul(&self.t),
            y: self.y.mul(&self.z),
            z: self.z.mul(&self.t),
        }
    }

    fn extended(&self) -> Point {
        Point {
            x: self.x.mul(&self.t),
            y: self.y.mul(&self.z),
            z: self.z.mul(&self.t),
            t: self.x.mul(&self.y),
        }
    }
}

impl Point {
    const IDENTITY: Point = Point {
        x: Fe::ZERO,
        y: Fe::ONE,
        z: Fe::ONE,
        t: Fe::ZERO,
    };

    /// Decodes a point. As in RFC 8032 the sign bit selects x, but two
    /// things the RFC forbids are accepted because deployed verifiers
    /// accept them: a y of p or more is reduced, and x = 0 may carry the
    /// sign bit. `None` when y is not on the curve.
    pub(super) fn decompress(bytes: &[u8; 32]) -> Option<Point> {
        let y = Fe::from_bytes(bytes);
        let yy = y.square();
        let u = yy.sub(&Fe::ONE);
        let v = yy.mul(&D).add(&Fe::ONE);
        let mut x = Fe::sqrt_ratio(&u, &v)?;
        if bytes[31] >> 7 == 1 {
            x = x.neg();
        }
        Some(Point {
            x,
            y,
            z: Fe::ONE,
            t: x.mul(&y),
        })
    }

    pub(super) fn compress(&self) -> [u8; 32] {
        let inverse = self.z.invert();
        let x = self.x.mul(&inverse);
        let mut bytes = self.y.mul(&inverse).to_bytes();
        bytes[31] ^= (x.to_bytes()[0] & 1) << 7;
        bytes
    }

    pub(super) fn neg(&self) -> Point {
        Point {
            x: self.x.neg(),
            y: self.y,
            z: self.z,
            t: self.t.neg(),
        }
    }

    fn projective(&self) -> Projective {
        Projective {
            x: self.x,
            y: self.y,
            z: self.z,
        }
    }

    fn niels(&self) -> ProjectiveNiels {
        ProjectiveNiels {
            y_plus_x: self.y.add(&self.x),
            y_minus_x: self.y.sub(&self.x),
            z: self.z,
            t2d: self.t.mul(&D2),
        }
    }

    fn add(&self, other: &ProjectiveNiels) -> Completed {
        let pp = self.y.add(&self.x).mul(&other.y_plus_x);
        let mm = self.y.sub(&self.x).mul(&other.y_minus_x);
        let tt2d = self.t.mul(&other.t2d);
        let zz = self.z.mul(&other.z);
        let zz2 = zz.add(&zz);
        Completed {
            x: pp.sub(&mm),
            y: pp.add(&mm),
            z: zz2.add(&tt2d),
            t: zz2.sub(&tt2d),
        }
    }

    fn sub(&self, other: &ProjectiveNiels) -> Completed {
        let pm = self.y.add(&self.x).mul(&other.y_minus_x);
        let mp = self.y.sub(&self.x).mul(&other.y_plus_x);
        let tt2d = self.t.mul(&other.t2d);
        let zz = self.z.mul(&other.z);
        let zz2 = zz.add(&zz);
        Completed {
            x: pm.sub(&mp),
            y: pm.add(&mp),
            z: zz2.sub(&tt2d),
            t: zz2.add(&tt2d),
        }
    }

    fn add_affine(&self, other: &AffineNiels) -> Completed {
        let pp = self.y.add(&self.x).mul(&other.y_plus_x);
        let mm = self.y.sub(&self.x).mul(&other.y_minus_x);
        let txy2d = self.t.mul(&other.xy2d);
        let z2 = self.z.add(&self.z);
        Completed {
            x: pp.sub(&mm),
            y: pp.add(&mm),
            z: z2.add(&txy2d),
            t: z2.sub(&txy2d),
        }
    }

    fn sub_affine(&self, other: &AffineNiels) -> Completed {
        let pm = self.y.add(&self.x).mul(&other.y_minus_x);
        let mp = self.y.sub(&self.x).mul(&other.y_plus_x);
        let txy2d = self.t.mul(&other.xy2d);
        let z2 = self.z.add(&self.z);
        Completed {
            x: pm.sub(&mp),
            y: pm.add(&mp),
            z: z2.sub(&txy2d),
            t: z2.add(&txy2d),
        }
    }

    /// whether eight times the point is the identity
    pub(super) fn is_small_order(&self) -> bool {
        let mut point = self.projective();
        for _ in 0..3 {
            point = point.double().projective();
        }
        point.x.is_zero() && point.y.sub(&point.z).is_zero()
    }

    /// `scalar * B` for a secret scalar below 2^255, in constant time: 64
    /// signed radix-16 digits, each a masked table lookup and one addition.
    pub(super) fn base_mul(scalar: &[u8; 32]) -> Point {
        let mut digits = scalar::radix_16(scalar);
        let mut point = Point::IDENTITY;
        for i in (1..64).step_by(2) {
            let entry = AffineNiels::select(&BASE_WINDOWS[i / 2], digits[i]);
            point = point.add_affine(&entry).extended();
        }
        // the odd digits were added a factor of 16 too low
        let mut scaled = point.projective();
        for _ in 0..3 {
            scaled = scaled.double().projective();
        }
        point = scaled.double().extended();
        for i in (0..64).step_by(2) {
            let entry = AffineNiels::select(&BASE_WINDOWS[i / 2], digits[i]);
            point = point.add_affine(&entry).extended();
        }
        wipe(&mut digits);
        point
    }

    /// `a * self + b * B`, compressed. Not constant time: for verification,
    /// where both scalars and the point are public.
    pub(super) fn double_base_mul_vartime(&self, a: &[u8; 32], b: &[u8; 32]) -> [u8; 32] {
        let a_naf = scalar::non_adjacent_form(a, 5);
        let b_naf = scalar::non_adjacent_form(b, 8);
        // self, 3 * self, ..., 15 * self
        let mut odd = [self.niels(); 8];
        let twice = self.projective().double().extended().niels();
        let mut multiple = *self;
        for slot in &mut odd[1..] {
            multiple = multiple.add(&twice).extended();
            *slot = multiple.niels();
        }
        let Some(top) = (0..256).rev().find(|&i| a_naf[i] != 0 || b_naf[i] != 0) else {
            return Point::IDENTITY.compress();
        };
        let mut sum = Projective::IDENTITY;
        for i in (0..=top).rev() {
            let mut step = sum.double();
            let a_digit = a_naf[i];
            if a_digit > 0 {
                step = step.extended().add(&odd[a_digit as usize / 2]);
            } else if a_digit < 0 {
                step = step
                    .extended()
                    .sub(&odd[a_digit.unsigned_abs() as usize / 2]);
            }
            let b_digit = b_naf[i];
            if b_digit > 0 {
                step = step.extended().add_affine(&BASE_ODD[b_digit as usize / 2]);
            } else if b_digit < 0 {
                step = step
                    .extended()
                    .sub_affine(&BASE_ODD[b_digit.unsigned_abs() as usize / 2]);
            }
            sum = step.projective();
        }
        Point {
            x: sum.x,
            y: sum.y,
            z: sum.z,
            t: Fe::ZERO,
        }
        .compress()
    }
}
