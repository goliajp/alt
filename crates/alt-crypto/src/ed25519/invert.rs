//! Inversion in GF(2^255 - 19) by division steps on the pair (p, x), 62 at a
//! time on machine words. Its running time depends on the value inverted,
//! so it serves verification only; signing keeps the fixed exponentiation.

use super::field::Fe;

const LIMB: i64 = (1 << 62) - 1;

/// 2^(-62 n) modulo p, for n up to the most batches a 255-bit input can take
static SCALE: [Fe; 13] = [
    Fe([1, 0, 0, 0, 0]),
    Fe([
        2251799813685246,
        2251799813685247,
        2251799813685247,
        592636767371263,
        237031559335289,
    ]),
    Fe([
        2251799813685233,
        2251799813685247,
        1540705163935743,
        1066642017008801,
        1777736695014669,
    ]),
    Fe([
        2251799813685240,
        1303673576357887,
        1422189356011735,
        118515779667644,
        948126237341157,
    ]),
    Fe([
        237031559335293,
        1896252474682314,
        1659220915347024,
        2014768254349958,
        355547339002933,
    ]),
    Fe([
        1777736695014652,
        711094678005867,
        1185157796676446,
        1740700513868530,
        2070907307876737,
    ]),
    Fe([
        948126237341155,
        829610457673512,
        424161928116547,
        1852588766383708,
        243269231949375,
    ]),
    Fe([
        355547339002917,
        1790212040335791,
        1759023677172409,
        1391000992941302,
        2002292909121785,
    ]),
    Fe([
        767233731532692,
        1397238665555389,
        274457595019808,
        536439844811444,
        24950690456346,
    ]),
    Fe([
        598816570952300,
        760996058918560,
        873274165972118,
        1297435903730004,
        1151671343695560,
    ]),
    Fe([
        1291198231115914,
        374260356845193,
        1909385699444004,
        391988479011544,
        453708608035138,
    ]),
    Fe([
        1447140046468069,
        801376782256984,
        354234144768389,
        719302141971771,
        1519037430809395,
    ]),
    Fe([
        428101320461817,
        1472419035483064,
        900851244897553,
        888047601110744,
        997699319695213,
    ]),
];

/// 62 division steps as one linear map: 2^62 f' = u f + v g, 2^62 g' = q f + r g
struct Batch {
    u: i64,
    v: i64,
    q: i64,
    r: i64,
}

/// Runs 62 division steps on the low words of f (odd) and g; only those
/// words decide which steps are taken.
fn batch(delta: &mut i64, mut f: u64, mut g: u64) -> Batch {
    let (mut u, mut v, mut q, mut r) = (1i64, 0i64, 0i64, 1i64);
    let mut left = 62u32;
    loop {
        // every trailing zero of g is a step that only halves it
        let zeros = g.trailing_zeros().min(left);
        g >>= zeros;
        u <<= zeros;
        v <<= zeros;
        *delta += i64::from(zeros);
        left -= zeros;
        if left == 0 {
            return Batch { u, v, q, r };
        }
        if *delta > 0 {
            *delta = 1 - *delta;
            (f, g) = (g, g.wrapping_sub(f) >> 1);
            (u, v, q, r) = (q << 1, r << 1, q - u, r - v);
        } else {
            *delta += 1;
            g = g.wrapping_add(f) >> 1;
            (u, v, q, r) = (u << 1, v << 1, q + u, r + v);
        }
        left -= 1;
    }
}

/// applies a batch to f and g, held as signed numbers on 62-bit limbs
fn advance(f: &mut [i64; 5], g: &mut [i64; 5], batch: &Batch) {
    let [u, v, q, r] = [batch.u, batch.v, batch.q, batch.r].map(i128::from);
    // the batch was chosen so that both sums are multiples of 2^62
    let mut carry_f = (u * i128::from(f[0]) + v * i128::from(g[0])) >> 62;
    let mut carry_g = (q * i128::from(f[0]) + r * i128::from(g[0])) >> 62;
    for i in 1..5 {
        carry_f += u * i128::from(f[i]) + v * i128::from(g[i]);
        carry_g += q * i128::from(f[i]) + r * i128::from(g[i]);
        f[i - 1] = carry_f as i64 & LIMB;
        g[i - 1] = carry_g as i64 & LIMB;
        carry_f >>= 62;
        carry_g >>= 62;
    }
    f[4] = carry_f as i64;
    g[4] = carry_g as i64;
}

/// `element` times a signed word of magnitude at most 2^62
fn scaled(element: &Fe, factor: i64) -> Fe {
    let magnitude = factor.unsigned_abs();
    let small = Fe([magnitude & ((1 << 51) - 1), magnitude >> 51, 0, 0, 0]);
    let product = element.mul(&small);
    if factor < 0 { product.neg() } else { product }
}

/// The inverse of `x`, or zero for zero. Not constant time.
pub(super) fn invert_vartime(x: &Fe) -> Fe {
    // the batches always suffice for inputs below p; the fixed exponentiation is always right
    by_division_steps(x).unwrap_or_else(|| x.invert())
}

/// `None` if g has not reached zero after the last batch `SCALE` provides for
fn by_division_steps(x: &Fe) -> Option<Fe> {
    let bytes = x.to_bytes();
    let mut words = [0u64; 4];
    for (word, chunk) in words.iter_mut().zip(bytes.as_chunks::<8>().0) {
        *word = u64::from_le_bytes(*chunk);
    }
    let mut g = [
        words[0] as i64 & LIMB,
        ((words[0] >> 62) | (words[1] << 2)) as i64 & LIMB,
        ((words[1] >> 60) | (words[2] << 4)) as i64 & LIMB,
        ((words[2] >> 58) | (words[3] << 6)) as i64 & LIMB,
        (words[3] >> 56) as i64,
    ];
    let mut f = [LIMB - 18, LIMB, LIMB, LIMB, 127];
    // d x = 2^(62 n) f and e x = 2^(62 n) g modulo p after n batches
    let (mut d, mut e) = (Fe::ZERO, Fe::ONE);
    let mut delta = 1;
    for scale in &SCALE[1..] {
        let step = batch(&mut delta, f[0] as u64, g[0] as u64);
        advance(&mut f, &mut g, &step);
        (d, e) = (
            scaled(&d, step.u).add(&scaled(&e, step.v)),
            scaled(&d, step.q).add(&scaled(&e, step.r)),
        );
        if g == [0; 5] {
            // f is now plus or minus the greatest common divisor, which is 1 unless x was zero
            return Some(if f == [1, 0, 0, 0, 0] {
                d.mul(scale)
            } else if f == [LIMB, LIMB, LIMB, LIMB, -1] {
                d.mul(scale).neg()
            } else {
                Fe::ZERO
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn element(seed: u64) -> Fe {
        let mut state = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
        let mut bytes = [0u8; 32];
        for byte in &mut bytes {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            *byte = state as u8;
        }
        Fe::from_bytes(&bytes)
    }

    fn check(x: &Fe) {
        let inverse = by_division_steps(x).expect("converges within the batches provided");
        assert_eq!(
            inverse.to_bytes(),
            x.invert().to_bytes(),
            "{:?}",
            x.to_bytes()
        );
        if !x.is_zero() {
            assert_eq!(inverse.mul(x).to_bytes(), Fe::ONE.to_bytes());
        }
    }

    #[test]
    fn matches_the_fixed_exponentiation() {
        for seed in 0..200_000 {
            check(&element(seed));
        }
    }

    #[test]
    fn handles_the_edges_of_the_field() {
        check(&Fe::ZERO);
        check(&Fe::ONE);
        check(&Fe::ONE.neg());
        // small values, values next to p, single bits and runs of ones
        for small in 2..200u64 {
            let x = Fe([small, 0, 0, 0, 0]);
            check(&x);
            check(&x.neg());
        }
        for limb in 0..5 {
            for bit in 0..51 {
                let mut limbs = [0u64; 5];
                limbs[limb] = 1 << bit;
                check(&Fe(limbs));
                check(&Fe(limbs).neg());
                let mut ones = [(1u64 << 51) - 1; 5];
                ones[limb] ^= 1 << bit;
                check(&Fe(ones));
            }
        }
        // inputs that are not reduced below p
        check(&Fe([
            (1 << 51) - 19,
            (1 << 51) - 1,
            (1 << 51) - 1,
            (1 << 51) - 1,
            (1 << 51) - 1,
        ]));
        check(&Fe([
            (1 << 51) - 18,
            (1 << 51) - 1,
            (1 << 51) - 1,
            (1 << 51) - 1,
            (1 << 51) - 1,
        ]));
        check(&Fe([(1 << 52) + 5, 1 << 52, 3, 1 << 53, (1 << 52) - 1]));
    }
}
