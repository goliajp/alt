//! Every backend of verification's double scalar multiplication must give
//! the same point, byte for byte.

use super::edge::TORSION;
use super::edge_cases::CASES;
use crate::{array, hex, noise, unhex};
use alt_crypto::ed25519::{Backend, SigningKey, double_base_mul};

const BASE: &str = "5866666666666666666666666666666666666666666666666666666666666666";
const IDENTITY: &str = "0100000000000000000000000000000000000000000000000000000000000000";

/// scalars below 2^255 that sit at the edges of the digit recoding
fn special_scalars() -> Vec<[u8; 32]> {
    let mut all = vec![[0u8; 32], [0x55; 32], [0x2a; 32]];
    for small in [1u8, 2, 15, 16, 17, 127, 128, 255] {
        let mut scalar = [0u8; 32];
        scalar[0] = small;
        all.push(scalar);
    }
    // the group order minus one, 2^252, and the largest value allowed
    let mut order_minus_one: [u8; 32] = array(&unhex(
        "ecd3f55c1a631258d69cf7a2def9de1400000000000000000000000000000010",
    ));
    all.push(order_minus_one);
    order_minus_one[0] += 2;
    all.push(order_minus_one);
    let mut power = [0u8; 32];
    power[31] = 0x10;
    all.push(power);
    let mut largest = [0xffu8; 32];
    largest[31] = 0x7f;
    all.push(largest);
    all
}

fn agree(point: &[u8; 32], a: &[u8; 32], b: &[u8; 32]) -> [u8; 32] {
    let mut results = Backend::available().iter().map(|&backend| {
        let result = double_base_mul(backend, point, a, b).expect("a point on the curve");
        (backend, result)
    });
    let (_, first) = results.next().unwrap();
    for (backend, result) in results {
        assert_eq!(
            hex(&result),
            hex(&first),
            "{backend:?} differs: point {} a {} b {}",
            hex(point),
            hex(a),
            hex(b)
        );
    }
    first
}

#[test]
fn trivial_scalars_give_the_expected_points() {
    let key = SigningKey::from_bytes(&[7; 32]).verifying_key().to_bytes();
    let (zero, mut one) = ([0u8; 32], [0u8; 32]);
    one[0] = 1;
    assert_eq!(hex(&agree(&key, &zero, &zero)), IDENTITY);
    assert_eq!(hex(&agree(&key, &zero, &one)), BASE);
    assert_eq!(agree(&key, &one, &zero), key);
    assert_eq!(hex(&agree(&array(&unhex(BASE)), &one, &zero)), BASE);
    assert_eq!(
        hex(&agree(&array(&unhex(IDENTITY)), &[0x55; 32], &one)),
        BASE
    );
    // a * B + b * B with a + b = L is the identity
    let order_minus_one: [u8; 32] = array(&unhex(
        "ecd3f55c1a631258d69cf7a2def9de1400000000000000000000000000000010",
    ));
    assert_eq!(
        hex(&agree(&array(&unhex(BASE)), &order_minus_one, &one)),
        IDENTITY
    );
}

#[test]
fn backends_agree_on_random_and_special_inputs() {
    println!("backends: {:?}", Backend::available());
    let random_scalar = |seed: u64| -> [u8; 32] {
        let mut scalar: [u8; 32] = array(&noise(32, seed));
        scalar[31] &= 0x7f;
        scalar
    };
    let mut compared = 0u32;
    // prime-order points
    for case in 0..2100u64 {
        let point = SigningKey::from_bytes(&array(&noise(32, 90_000 + case)))
            .verifying_key()
            .to_bytes();
        for pair in 0..5 {
            let seed = 100_000 + 10 * case + 2 * pair;
            agree(&point, &random_scalar(seed), &random_scalar(seed + 1));
            compared += 1;
        }
    }
    // points of small order (the identity among them) and of mixed order
    let mut odd_points: Vec<[u8; 32]> = TORSION.iter().map(|text| array(&unhex(text))).collect();
    odd_points.extend(
        CASES
            .iter()
            .map(|case| -> [u8; 32] { array(&unhex(case.key)) }),
    );
    let specials = special_scalars();
    for (index, point) in odd_points.iter().enumerate() {
        for (i, a) in specials.iter().enumerate() {
            for b in [&specials[(i + 3) % specials.len()], &[0u8; 32]] {
                agree(point, a, b);
                compared += 1;
            }
        }
        for pair in 0..10 {
            let seed = 200_000 + 100 * index as u64 + 2 * pair;
            agree(point, &random_scalar(seed), &random_scalar(seed + 1));
            compared += 1;
        }
    }
    // special scalars against each other on an ordinary point
    let point = SigningKey::from_bytes(&[9; 32]).verifying_key().to_bytes();
    for a in &specials {
        for b in &specials {
            agree(&point, a, b);
            compared += 1;
        }
    }
    println!("{compared} double scalar multiplications compared");
    assert!(compared >= 10_000);
}
