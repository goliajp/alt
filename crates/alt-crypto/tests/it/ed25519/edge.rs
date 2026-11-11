//! Every boundary case is put to this implementation and to ed25519-dalek;
//! key parsing, `verify` and `verify_strict` must agree on each one.

use super::edge_cases::CASES;
use crate::{array, hex, noise, unhex};
use alt_crypto::ed25519::{Signature, SigningKey, VerifyingKey};
use ed25519_dalek::Verifier;

/// the group order L, little-endian
const ORDER: [u8; 32] = [
    0xed, 0xd3, 0xf5, 0x5c, 0x1a, 0x63, 0x12, 0x58, 0xd6, 0x9c, 0xf7, 0xa2, 0xde, 0xf9, 0xde, 0x14,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x10,
];

/// the eight points of small order in canonical form, then the six other
/// encodings that decode to one of them
const TORSION: [&str; 14] = [
    "0100000000000000000000000000000000000000000000000000000000000000",
    "ecffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f",
    "0000000000000000000000000000000000000000000000000000000000000000",
    "0000000000000000000000000000000000000000000000000000000000000080",
    "26e8958fc2b227b045c3f489f2ef98f0d5dfac05d3c63339b13802886d53fc05",
    "26e8958fc2b227b045c3f489f2ef98f0d5dfac05d3c63339b13802886d53fc85",
    "c7176a703d4dd84fba3c0b760d10670f2a2053fa2c39ccc64ec7fd7792ac037a",
    "c7176a703d4dd84fba3c0b760d10670f2a2053fa2c39ccc64ec7fd7792ac03fa",
    // x = 0 with the sign bit: the identity and the point of order 2
    "0100000000000000000000000000000000000000000000000000000000000080",
    "ecffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
    // y = p + 1 for the identity and y = p for the points of order 4
    "eeffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f",
    "eeffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
    "edffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f",
    "edffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
];

#[derive(Default)]
struct Tally {
    cases: usize,
    keys_rejected: usize,
    verify_accepted: usize,
    strict_accepted: usize,
}

impl Tally {
    /// Runs one case through both implementations and returns what they agreed on:
    /// `None` when the key does not parse, else (verify, verify_strict).
    fn compare(
        &mut self,
        label: &str,
        key: &[u8; 32],
        message: &[u8],
        signature: &[u8; 64],
    ) -> Option<(bool, bool)> {
        self.cases += 1;
        let ours = VerifyingKey::from_bytes(key);
        let theirs = ed25519_dalek::VerifyingKey::from_bytes(key);
        assert_eq!(
            ours.is_ok(),
            theirs.is_ok(),
            "{label}: key {} parses",
            hex(key)
        );
        let (Ok(ours), Ok(theirs)) = (ours, theirs) else {
            self.keys_rejected += 1;
            return None;
        };
        assert_eq!(
            ours.to_bytes(),
            theirs.to_bytes(),
            "{label}: key bytes are kept as given"
        );
        assert_eq!(
            ours.is_weak(),
            theirs.is_weak(),
            "{label}: key {} is weak",
            hex(key)
        );
        let our_signature = Signature::from_bytes(signature);
        let their_signature = ed25519_dalek::Signature::from_bytes(signature);
        let verify = ours.verify(message, &our_signature).is_ok();
        let strict = ours.verify_strict(message, &our_signature).is_ok();
        let context = || format!("{label}: key {} signature {}", hex(key), hex(signature));
        assert_eq!(
            verify,
            theirs.verify(message, &their_signature).is_ok(),
            "verify, {}",
            context()
        );
        assert_eq!(
            strict,
            theirs.verify_strict(message, &their_signature).is_ok(),
            "verify_strict, {}",
            context()
        );
        self.verify_accepted += usize::from(verify);
        self.strict_accepted += usize::from(strict);
        Some((verify, strict))
    }
}

fn join(r: &[u8], s: &[u8]) -> [u8; 64] {
    let mut out = [0u8; 64];
    out[..32].copy_from_slice(r);
    out[32..].copy_from_slice(s);
    out
}

/// little-endian `a + b`, dropping a carry out of the top
fn add(a: &[u8; 32], b: &[u8; 32]) -> [u8; 32] {
    let mut out = [0u8; 32];
    let mut carry = 0u16;
    for i in 0..32 {
        carry += u16::from(a[i]) + u16::from(b[i]);
        out[i] = carry as u8;
        carry >>= 8;
    }
    out
}

fn small_order_grid(tally: &mut Tally) {
    let zero = [0u8; 32];
    let mut accepted_by_key = [0usize; 14];
    for (index, key) in TORSION.iter().enumerate() {
        let key: [u8; 32] = array(&unhex(key));
        for r in TORSION {
            let r = unhex(r);
            for seed in 0..24u64 {
                let message = noise(seed as usize, seed + 1);
                // with S = 0 the check reduces to R = -k * A, which a small-order pair can satisfy
                let outcome = tally.compare("small order, S = 0", &key, &message, &join(&r, &zero));
                let (verify, strict) = outcome.expect("small-order points are on the curve");
                assert!(!strict, "verify_strict accepted a small-order key");
                accepted_by_key[index] += usize::from(verify);
            }
            // S = L is zero modulo the order but not canonical
            let outcome = tally.compare("small order, S = L", &key, b"", &join(&r, &ORDER));
            assert_eq!(outcome, Some((false, false)));
        }
    }
    // k depends on R, so a pair matches by chance (always, for the identity key); every
    // key must have been accepted at least once for the grid to reach the accepting branch
    assert!(
        accepted_by_key.iter().all(|&count| count > 0),
        "{accepted_by_key:?}"
    );
}

fn non_canonical_y(tally: &mut Tally) {
    let honest = SigningKey::from_bytes(&[7; 32]);
    let public = honest.verifying_key().to_bytes();
    let signature = honest.sign(b"message").to_bytes();
    let identity: [u8; 32] = array(&unhex(TORSION[0]));
    // p, p + 1, ..., 2^255 - 1 are the nineteen values a canonical y never takes
    for extra in 0..19u8 {
        for sign in [0u8, 0x80] {
            let mut encoding = [0xffu8; 32];
            encoding[0] = 0xed + extra;
            encoding[31] = 0x7f | sign;
            tally.compare(
                "non-canonical y as key",
                &encoding,
                b"message",
                &join(&identity, &[0; 32]),
            );
            tally.compare("non-canonical y as key", &encoding, b"message", &signature);
            tally.compare(
                "non-canonical y as R",
                &public,
                b"message",
                &join(&encoding, &signature[32..]),
            );
        }
    }
}

fn scalar_range(tally: &mut Tally) {
    let honest = SigningKey::from_bytes(&[7; 32]);
    let public = honest.verifying_key().to_bytes();
    let signature = honest.sign(b"message").to_bytes();
    let (r, s) = signature.split_at(32);
    let s: [u8; 32] = array(s);
    assert_eq!(
        tally.compare("valid", &public, b"message", &signature),
        Some((true, true))
    );
    let mut order_minus_one = ORDER;
    order_minus_one[0] -= 1;
    let mut order_plus_one = ORDER;
    order_plus_one[0] += 1;
    let mut candidates = vec![
        add(&s, &ORDER),
        add(&add(&s, &ORDER), &ORDER),
        ORDER,
        order_minus_one,
        order_plus_one,
        [0; 32],
        [0xff; 32],
    ];
    for bit in [252, 253, 254, 255] {
        let mut high = s;
        high[bit / 8] |= 1 << (bit % 8);
        candidates.push(high);
        let mut power = [0u8; 32];
        power[bit / 8] = 1 << (bit % 8);
        candidates.push(power);
    }
    for candidate in candidates {
        let outcome = tally.compare("S out of range", &public, b"message", &join(r, &candidate));
        assert_eq!(outcome, Some((false, false)), "S = {}", hex(&candidate));
    }
}

fn corruption(tally: &mut Tally) {
    for case in 0..300u64 {
        let key = SigningKey::from_bytes(&array(&noise(32, 5000 + case)));
        let public = key.verifying_key().to_bytes();
        let message = noise(case as usize % 90, 6000 + case);
        let signature = key.sign(&message).to_bytes();
        let pick = noise(8, 7000 + case);
        let mut bad_signature = signature;
        bad_signature[pick[0] as usize % 64] ^= 1 << (pick[1] % 8);
        tally.compare("flipped signature bit", &public, &message, &bad_signature);
        let mut bad_key = public;
        bad_key[pick[2] as usize % 32] ^= 1 << (pick[3] % 8);
        tally.compare("flipped key bit", &bad_key, &message, &signature);
        let mut bad_message = message.clone();
        bad_message.push(pick[4]);
        tally.compare("extended message", &public, &bad_message, &signature);
        // arbitrary bytes as a key, and as R: about half are not on the curve
        let random: [u8; 32] = array(&noise(32, 8000 + case));
        tally.compare("random key", &random, &message, &signature);
        tally.compare(
            "random R",
            &public,
            &message,
            &join(&random, &signature[32..]),
        );
    }
}

fn generated(tally: &mut Tally) {
    for case in &CASES {
        let outcome = tally.compare(
            case.name,
            &array(&unhex(case.key)),
            &unhex(case.message),
            &array(&unhex(case.signature)),
        );
        // the fixture's own expectation is a third opinion beside the two implementations
        assert_eq!(outcome, Some((case.verify, case.strict)), "{}", case.name);
    }
}

#[test]
fn verify_and_verify_strict_agree_with_dalek_on_every_boundary_case() {
    let mut tally = Tally::default();
    small_order_grid(&mut tally);
    non_canonical_y(&mut tally);
    scalar_range(&mut tally);
    corruption(&mut tally);
    generated(&mut tally);
    println!(
        "boundary cases: {} compared, {} keys rejected by both, verify accepted {}, verify_strict accepted {}",
        tally.cases, tally.keys_rejected, tally.verify_accepted, tally.strict_accepted
    );
    assert!(tally.keys_rejected > 100 && tally.verify_accepted > 300 && tally.strict_accepted >= 3);
}
