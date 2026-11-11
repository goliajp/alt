//! 128-bit significands of the powers of ten, computed at compile time.

pub(super) const MIN_EXP: i32 = -292;
pub(super) const MAX_EXP: i32 = 324;
const COUNT: usize = (MAX_EXP - MIN_EXP + 1) as usize;
const LIMBS: usize = 16;

/// `SIGNIFICANDS[e - MIN_EXP]` is the leading 128 bits of 10^e, rounded down,
/// with the top bit set.
pub(super) static SIGNIFICANDS: [u128; COUNT] = build();

type Big = [u64; LIMBS];

const fn build() -> [u128; COUNT] {
    let mut table = [0u128; COUNT];
    let zero = -MIN_EXP as usize;

    // 10^e and 5^e share their leading bits, and 5^e is exact here
    let mut power = [0u64; LIMBS];
    power[0] = 1;
    let mut e = 0;
    while e <= MAX_EXP as usize {
        table[zero + e] = leading_128(&power);
        power = times_5(power);
        e += 1;
    }

    // floor(floor(x / 5) / 5) = floor(x / 25), so repeated short division
    // keeps floor(2^1023 / 5^e) exact, and its leading bits are those of 10^-e
    let mut quotient = [0u64; LIMBS];
    quotient[LIMBS - 1] = 1 << 63;
    let mut e = 1;
    while e <= zero {
        quotient = div_5(quotient);
        table[zero - e] = leading_128(&quotient);
        e += 1;
    }
    table
}

const fn times_5(mut n: Big) -> Big {
    let mut carry = 0u128;
    let mut i = 0;
    while i < LIMBS {
        let product = n[i] as u128 * 5 + carry;
        n[i] = product as u64;
        carry = product >> 64;
        i += 1;
    }
    assert!(carry == 0);
    n
}

const fn div_5(mut n: Big) -> Big {
    let mut remainder = 0u128;
    let mut i = LIMBS;
    while i > 0 {
        i -= 1;
        let current = (remainder << 64) | n[i] as u128;
        n[i] = (current / 5) as u64;
        remainder = current % 5;
    }
    n
}

/// The 128 bits starting at the highest set bit, shifted up when the number
/// is shorter than that.
const fn leading_128(n: &Big) -> u128 {
    let mut top = LIMBS - 1;
    while n[top] == 0 {
        top -= 1;
    }
    let a = n[top];
    let b = if top >= 1 { n[top - 1] } else { 0 };
    let c = if top >= 2 { n[top - 2] } else { 0 };
    let shift = a.leading_zeros();
    let joined = ((a as u128) << 64) | b as u128;
    if shift == 0 {
        joined
    } else {
        (joined << shift) | (c >> (64 - shift)) as u128
    }
}
