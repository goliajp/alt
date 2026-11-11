//! The double scalar multiplication of verification on AVX2, with the four
//! coordinates of a point in one set of vectors.

mod field;
mod point;

use super::field::Fe;
use super::tables::BASE_ODD;
use point::{Cached, Extended, pack_table};

/// `BASE_ODD` in the layout of [`Cached`]
static BASE_ODD_PACKED: [[[i32; 8]; 5]; 64] = pack_table(&BASE_ODD);

/// `a * P + b * B` for `P` given by extended coordinates on 51-bit limbs
/// below 2^51 and the scalars in non-adjacent form (widths 5 and 8).
/// Returns (X, Y, Z). Not constant time.
#[target_feature(enable = "avx2")]
pub(super) fn double_base_mul_vartime(
    point: [Fe; 4],
    a_naf: &[i8; 256],
    b_naf: &[i8; 256],
) -> [Fe; 3] {
    let point = Extended::new(point);
    // point, 3 * point, ..., 15 * point
    let mut odd = [point.cached(); 8];
    let twice = point.double().cached();
    let mut multiple = point;
    for slot in &mut odd[1..] {
        multiple = multiple.add(&twice);
        *slot = multiple.cached();
    }
    let mut sum = Extended::identity();
    let top = (0..256).rev().find(|&i| a_naf[i] != 0 || b_naf[i] != 0);
    for i in (0..=top.unwrap_or(0)).rev() {
        sum = sum.double();
        let a_digit = a_naf[i];
        if a_digit > 0 {
            sum = sum.add(&odd[a_digit as usize / 2]);
        } else if a_digit < 0 {
            sum = sum.sub(&odd[a_digit.unsigned_abs() as usize / 2]);
        }
        let b_digit = b_naf[i];
        if b_digit > 0 {
            sum = sum.add(&Cached::from_packed(&BASE_ODD_PACKED[b_digit as usize / 2]));
        } else if b_digit < 0 {
            let entry = &BASE_ODD_PACKED[b_digit.unsigned_abs() as usize / 2];
            sum = sum.sub(&Cached::from_packed(entry));
        }
    }
    let [x, y, z, _] = sum.coordinates();
    [x, y, z]
}
