//! A base-two logarithm that needs no C library.

/// Base-two logarithm of a positive, finite, normal `x`, from arithmetic
/// alone: the C library's `log2` would be the only thing in the binary that
/// needs libm loaded.
pub(crate) fn log2(x: f64) -> f64 {
    let bits = x.to_bits();
    let mut exponent = ((bits >> 52) & 0x7ff) as i32 - 1023;
    // the mantissa as a number in [1, 2), then in [sqrt(1/2), sqrt(2))
    let mut m = f64::from_bits(bits & ((1 << 52) - 1) | (1023 << 52));
    if m > std::f64::consts::SQRT_2 {
        m /= 2.0;
        exponent += 1;
    }
    // ln m = 2 atanh z with z = (m - 1) / (m + 1), |z| < 0.172: fourteen
    // terms of the series leave less than the last bit
    let z = (m - 1.0) / (m + 1.0);
    let (mut power, mut sum) = (z, 0.0);
    for k in 0..14 {
        sum += power / (2 * k + 1) as f64;
        power *= z * z;
    }
    exponent as f64 + 2.0 * sum * std::f64::consts::LOG2_E
}

#[cfg(test)]
mod tests {
    use super::log2;

    #[test]
    fn agrees_with_the_c_library_on_probabilities() {
        for count in 1..=64u32 {
            for of in [count, 2 * count, 7 * count + 3, 64, 1000] {
                let p = count as f64 / of.max(count) as f64;
                let (ours, theirs) = (log2(p), p.log2());
                assert!((ours - theirs).abs() <= 4.0 * f64::EPSILON, "log2({p})");
            }
        }
    }
}
