//! Shortest decimal text that reads back as the same `f64`.
//!
//! The digits are the shortest that identify the value, closest to it when
//! several have that length. Values whose decimal exponent is between -5 and
//! 15 (-6 and 12 for `f32`) are written positionally (`1500.0`, `0.00012`),
//! the rest as `1.5e+16` / `1.2e-7`.

mod pow10;
mod single;

pub(crate) use single::write_f32;

use std::ops::RangeInclusive;

use crate::digits::PAIRS;

const FRACTION_BITS: u32 = 52;
const IMPLICIT_BIT: u64 = 1 << FRACTION_BITS;
/// exponent of the last significand bit for a biased exponent of zero
const EXP_OFFSET: i32 = 1075;
/// smallest significand with seventeen digits
const SEVENTEEN_DIGITS: u64 = 10_000_000_000_000_000;

struct Decimal {
    sig: u64,
    exp: i32,
}

/// Appends the text of a finite `value`.
pub(crate) fn write_f64(value: f64, out: &mut Vec<u8>) {
    let bits = value.to_bits();
    let raw_exp = ((bits >> FRACTION_BITS) & 0x7ff) as i32;
    let fraction = bits & (IMPLICIT_BIT - 1);
    if bits >> 63 != 0 {
        out.push(b'-');
    }
    let decimal = if raw_exp == 0 {
        if fraction == 0 {
            out.extend_from_slice(b"0.0");
            return;
        }
        let mut decimal = to_decimal_exact(fraction, 1 - EXP_OFFSET, true);
        while decimal.sig < SEVENTEEN_DIGITS {
            decimal.sig *= 10;
            decimal.exp -= 1;
        }
        decimal
    } else {
        // a power of two is closer to its lower neighbour than to its upper
        let regular = fraction != 0;
        let sig = fraction | IMPLICIT_BIT;
        let bin_exp = raw_exp - EXP_OFFSET;
        match to_decimal_fast(sig, bin_exp, regular) {
            Some(decimal) => decimal,
            None => to_decimal_exact(sig, bin_exp, regular),
        }
    };
    let extra_digit = decimal.sig >= SEVENTEEN_DIGITS;
    let exponent = decimal.exp + 15 + i32::from(extra_digit);
    let mut buf = [0u8; 17];
    let digits = significant_digits(decimal.sig, extra_digit, &mut buf);
    layout(digits, exponent, -5..=15, out);
}

/// floor(log10(2^bin_exp)), or floor(log10(3/4 * 2^bin_exp)) for a power of two
const fn decimal_exponent(bin_exp: i32, regular: bool) -> i32 {
    (bin_exp * 315_653 - (!regular) as i32 * 131_072) >> 20
}

/// Left shift that puts the scaled significand's binary point at bit 64 of
/// the 192-bit product with the power of ten.
const fn exponent_shift(bin_exp: i32, dec_exp: i32) -> u32 {
    let pow10_bin_exp = (-dec_exp * 217_707) >> 16;
    (bin_exp + pow10_bin_exp + 1) as u32
}

fn pow10_significand(exp: i32) -> u128 {
    pow10::SIGNIFICANDS[(exp - pow10::MIN_EXP) as usize]
}

/// Upper 128 bits of the 192-bit product, as (high, low) words.
fn mul_high_128(x: u128, y: u64) -> (u64, u64) {
    let high = (x >> 64) * u128::from(y);
    let low = (u128::from(x as u64) * u128::from(y)) >> 64;
    let sum = high + low;
    ((sum >> 64) as u64, sum as u64)
}

/// Upper 64 bits of the 192-bit product, with every discarded bit except the
/// lowest folded into bit zero.
fn mul_high_to_odd(x: u128, y: u64) -> u64 {
    let (high, low) = mul_high_128(x, y);
    high | u64::from(low >> 1 != 0)
}

/// Handles most values that are not powers of two with one multiplication;
/// returns `None` when the result is too close to a rounding boundary to
/// decide without the exact method.
fn to_decimal_fast(sig: u64, bin_exp: i32, regular: bool) -> Option<Decimal> {
    const HALF: u64 = 1 << 63;
    const TEN: u64 = 10 << 60;
    if !regular {
        return None;
    }
    let dec_exp = decimal_exponent(bin_exp, true);
    let shift = exponent_shift(bin_exp, dec_exp);
    let pow10 = pow10_significand(-dec_exp);
    let (integral, fractional) = mul_high_128(pow10, sig << shift);
    if fractional == HALF {
        return None;
    }
    // fixed point with four integral bits: the last integral digit and the
    // fraction, against half the distance to the neighbouring floats
    let digit = integral % 10;
    let scaled_mod_10 = (digit << 60) | (fractional >> 4);
    let half_ulp = ((pow10 >> 64) as u64) >> (5 - shift);
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
    Some(Decimal { sig, exp: dec_exp })
}

/// Schubfach: scales the value and both ends of its rounding interval by a
/// power of ten, then picks a multiple of ten inside the interval if there is
/// one, otherwise the closer of the two neighbouring integers.
fn to_decimal_exact(sig: u64, bin_exp: i32, regular: bool) -> Decimal {
    let dec_exp = decimal_exponent(bin_exp, regular);
    let shift = exponent_shift(bin_exp, dec_exp);
    // strictly above the true power of ten, which rounding to odd relies on
    let pow10 = pow10_significand(-dec_exp) + 1;

    // two extra bits so the interval ends are integers
    let scaled_sig = sig << 2;
    let odd = sig & 1;
    let lower = mul_high_to_odd(pow10, (scaled_sig - (u64::from(regular) + 1)) << shift) + odd;
    let upper = mul_high_to_odd(pow10, (scaled_sig + 2) << shift) - odd;

    let shorter = (upper >> 2) / 10 * 10;
    if shorter << 2 >= lower {
        return Decimal {
            sig: shorter,
            exp: dec_exp,
        };
    }

    let scaled = mul_high_to_odd(pow10, scaled_sig << shift);
    let below = scaled >> 2;
    let above = below + 1;
    let distance = scaled.wrapping_sub((below + above) << 1) as i64;
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

/// Writes the sixteen or seventeen digits of `sig` and returns them without
/// trailing zeros.
fn significant_digits(mut sig: u64, extra_digit: bool, buf: &mut [u8; 17]) -> &[u8] {
    let mut at = buf.len();
    while at > 1 {
        let pair = (sig % 100) as usize * 2;
        sig /= 100;
        at -= 2;
        buf[at..at + 2].copy_from_slice(&PAIRS[pair..pair + 2]);
    }
    let start = if extra_digit {
        buf[0] = b'0' + sig as u8;
        0
    } else {
        1
    };
    let mut end = buf.len();
    while buf[end - 1] == b'0' {
        end -= 1;
    }
    &buf[start..end]
}

/// `exponent` is the power of ten of the first digit; values whose exponent
/// is in `positional` are written without an exponent.
fn layout(digits: &[u8], exponent: i32, positional: RangeInclusive<i32>, out: &mut Vec<u8>) {
    if positional.contains(&exponent) {
        if exponent < 0 {
            // 1234e-6 -> 0.001234
            out.extend_from_slice(b"0.");
            out.resize(out.len() + (-exponent - 1) as usize, b'0');
            out.extend_from_slice(digits);
            return;
        }
        let integral = exponent as usize + 1;
        if digits.len() <= integral {
            // 1234e7 -> 12340000000.0
            out.extend_from_slice(digits);
            out.resize(out.len() + integral - digits.len(), b'0');
            out.extend_from_slice(b".0");
        } else {
            // 1234e-2 -> 12.34
            out.extend_from_slice(&digits[..integral]);
            out.push(b'.');
            out.extend_from_slice(&digits[integral..]);
        }
        return;
    }
    // 1234e30 -> 1.234e+33
    out.push(digits[0]);
    if digits.len() > 1 {
        out.push(b'.');
        out.extend_from_slice(&digits[1..]);
    }
    out.push(b'e');
    out.push(if exponent < 0 { b'-' } else { b'+' });
    crate::digits::write_u64(u64::from(exponent.unsigned_abs()), out);
}
