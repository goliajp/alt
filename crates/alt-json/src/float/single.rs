//! The same conversion for `f32`: shortest among single-precision values.

use super::{decimal_exponent, exponent_shift, layout, pow10_significand};

const FRACTION_BITS: u32 = 23;
const IMPLICIT_BIT: u32 = 1 << FRACTION_BITS;
const EXP_OFFSET: i32 = 150;

struct Decimal {
    sig: u32,
    exp: i32,
}

/// Appends the text of a finite `value`.
pub(crate) fn write_f32(value: f32, out: &mut Vec<u8>) {
    let bits = value.to_bits();
    let raw_exp = ((bits >> FRACTION_BITS) & 0xff) as i32;
    let fraction = bits & (IMPLICIT_BIT - 1);
    if bits >> 31 != 0 {
        out.push(b'-');
    }
    let decimal = if raw_exp == 0 {
        if fraction == 0 {
            out.extend_from_slice(b"0.0");
            return;
        }
        to_decimal_exact(fraction, 1 - EXP_OFFSET, true)
    } else {
        let regular = fraction != 0;
        let sig = fraction | IMPLICIT_BIT;
        let bin_exp = raw_exp - EXP_OFFSET;
        match to_decimal_fast(sig, bin_exp, regular) {
            Some(decimal) => decimal,
            None => to_decimal_exact(sig, bin_exp, regular),
        }
    };
    let mut buf = [0u8; 10];
    let mut at = buf.len();
    let mut sig = decimal.sig;
    while sig != 0 {
        at -= 1;
        buf[at] = b'0' + (sig % 10) as u8;
        sig /= 10;
    }
    let exponent = decimal.exp + (buf.len() - at) as i32 - 1;
    let mut end = buf.len();
    while buf[end - 1] == b'0' {
        end -= 1;
    }
    layout(&buf[at..end], exponent, -6..=12, out);
}

/// Upper 32 bits of the product with the high word of the power of ten,
/// with every discarded bit except the lowest folded into bit zero.
fn mul_high_to_odd(pow10_high: u64, y: u32) -> u32 {
    let p = ((u128::from(pow10_high) * u128::from(y)) >> 32) as u64;
    (p >> 32) as u32 | u32::from(p as u32 >> 1 != 0)
}

fn to_decimal_fast(sig: u32, bin_exp: i32, regular: bool) -> Option<Decimal> {
    const HALF: u64 = 1 << 63;
    const TEN: u64 = 10 << 32;
    if !regular {
        return None;
    }
    let dec_exp = decimal_exponent(bin_exp, true);
    let shift = exponent_shift(bin_exp, dec_exp);
    let pow10_high = (pow10_significand(-dec_exp) >> 64) as u64;
    let product = u128::from(pow10_high) * u128::from(sig << shift);
    let integral = u64::from((product >> 64) as u32);
    let fractional = product as u64;
    if fractional == HALF {
        return None;
    }
    // fixed point with 32 integral bits
    let digit = integral % 10;
    let scaled_mod_10 = (digit << 32) | (fractional >> 32);
    let half_ulp = pow10_high >> (33 - shift);
    let upper = scaled_mod_10 + half_ulp;
    if TEN.wrapping_sub(upper) <= 1 || scaled_mod_10 == half_ulp {
        return None;
    }
    let shorter = integral - digit;
    let sig = if TEN < upper {
        shorter + 10
    } else if scaled_mod_10 < half_ulp {
        shorter
    } else {
        integral + u64::from(fractional >= HALF)
    };
    Some(Decimal {
        sig: sig as u32,
        exp: dec_exp,
    })
}

fn to_decimal_exact(sig: u32, bin_exp: i32, regular: bool) -> Decimal {
    let dec_exp = decimal_exponent(bin_exp, regular);
    let shift = exponent_shift(bin_exp, dec_exp);
    // strictly above the true power of ten
    let pow10_high = (pow10_significand(-dec_exp) >> 64) as u64 + 1;

    let scaled_sig = sig << 2;
    let odd = sig & 1;
    let lower = mul_high_to_odd(pow10_high, (scaled_sig - (u32::from(regular) + 1)) << shift) + odd;
    let upper = mul_high_to_odd(pow10_high, (scaled_sig + 2) << shift) - odd;

    let shorter = (upper >> 2) / 10 * 10;
    if shorter << 2 >= lower {
        return Decimal {
            sig: shorter,
            exp: dec_exp,
        };
    }

    let scaled = mul_high_to_odd(pow10_high, scaled_sig << shift);
    let below = scaled >> 2;
    let above = below + 1;
    let distance = scaled.wrapping_sub((below + above) << 1) as i32;
    let below_closer = distance < 0 || (distance == 0 && below & 1 == 0);
    let below_inside = below << 2 >= lower;
    Decimal {
        sig: if below_closer && below_inside {
            below
        } else {
            above
        },
        exp: dec_exp,
    }
}
