//! Comparison whose running time does not depend on the bytes compared.

use std::hint::black_box;

/// Whether `a` and `b` hold the same bytes.
///
/// Slices of different lengths compare unequal at once; for equal lengths
/// every byte is read whatever the outcome.
pub fn eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut difference = 0u8;
    for (x, y) in a.iter().zip(b) {
        difference |= x ^ y;
    }
    // hides the accumulator from the optimizer so the loop cannot become an early exit
    black_box(difference) == 0
}
