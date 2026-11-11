//! Expected values under `fixtures/` were produced by ed25519-dalek 2.2 in the run where it
//! agreed with this implementation on every case; `reference.rs` says how to regenerate them.

pub(crate) mod edge;
mod edge_cases;
mod vectors;

use super::{array, hex, noise, unhex};
use alt_crypto::ed25519::{Error, Signature, SigningKey, VerifyingKey};

#[test]
fn rfc_8032_vectors() {
    for (secret, public, message, signature) in vectors::RFC_8032 {
        let key = SigningKey::from_bytes(&array(&unhex(secret)));
        let message = unhex(message);
        assert_eq!(hex(&key.verifying_key().to_bytes()), public);
        assert_eq!(hex(&key.to_bytes()), secret);
        let signed = key.sign(&message);
        assert_eq!(signed.to_bytes().to_vec(), unhex(signature));
        let verifying = VerifyingKey::from_bytes(&array(&unhex(public))).unwrap();
        assert_eq!(verifying.verify(&message, &signed), Ok(()));
        assert_eq!(verifying.verify_strict(&message, &signed), Ok(()));
    }
}

/// (seed, message) pairs for the recorded public keys and signatures: every
/// message length around a SHA-512 block boundary, long messages, extreme seeds
pub(crate) fn signing_cases() -> Vec<([u8; 32], Vec<u8>)> {
    let mut cases = Vec::new();
    for (index, length) in [0, 1, 63, 64, 65, 127, 128, 129, 1024, 65_536]
        .into_iter()
        .enumerate()
    {
        let index = index as u64;
        cases.push((array(&noise(32, 100 + index)), noise(length, 200 + index)));
    }
    for seed in [[0u8; 32], [0xff; 32], [0x80; 32], [1; 32]] {
        for length in [0, 1, 64, 129, 200] {
            cases.push((seed, noise(length, 300 + length as u64)));
        }
    }
    for case in 0..260u64 {
        cases.push((
            array(&noise(32, 1000 + 2 * case)),
            noise((case as usize * 7) % 400, 1001 + 2 * case),
        ));
    }
    cases
}

#[test]
fn public_keys_and_signatures_match_the_recorded_ones() {
    // 32 bytes of public key and 64 of signature per case, written by `reference.rs`
    let (expected, rest) = include_bytes!("fixtures/ed25519_sign.bin").as_chunks::<96>();
    let cases = signing_cases();
    assert!(rest.is_empty() && expected.len() == cases.len() && cases.len() >= 256);
    for (index, ((seed, message), expected)) in cases.iter().zip(expected).enumerate() {
        let key = SigningKey::from_bytes(seed);
        let public = key.verifying_key();
        assert_eq!(
            public.to_bytes(),
            expected[..32],
            "public key, case {index}"
        );
        let signature = key.sign(message);
        assert_eq!(
            signature.to_bytes(),
            expected[32..],
            "signature, case {index}"
        );
        assert_eq!(public.verify(message, &signature), Ok(()));
        assert_eq!(public.verify_strict(message, &signature), Ok(()));
    }
}

#[test]
fn keys_and_signatures_round_trip_through_bytes() {
    let key = SigningKey::from_bytes(&[7; 32]);
    let public = key.verifying_key();
    assert_eq!(VerifyingKey::from_bytes(public.as_bytes()), Ok(public));
    assert_eq!(*public.as_bytes(), public.to_bytes());
    assert_eq!(key.clone().to_bytes(), [7; 32]);
    let signature = key.sign(b"message");
    assert_eq!(Signature::from_bytes(&signature.to_bytes()), signature);
    assert_eq!(Signature::from_slice(&signature.to_bytes()), Ok(signature));
    for length in [0, 1, 63, 65, 128] {
        assert_eq!(
            Signature::from_slice(&vec![0; length]),
            Err(Error::SignatureLength)
        );
    }
    assert!(!format!("{key:?}").contains("0707"));
    assert_eq!(
        format!("{public:?}"),
        format!("VerifyingKey({})", hex(public.as_bytes()))
    );
}

#[test]
fn rejects_a_signature_for_another_message_or_key() {
    let key = SigningKey::from_bytes(&[7; 32]);
    let other = SigningKey::from_bytes(&[8; 32]);
    let signature = key.sign(b"message");
    assert_eq!(key.verifying_key().verify(b"message", &signature), Ok(()));
    assert_eq!(
        key.verifying_key().verify(b"messagf", &signature),
        Err(Error::Verification)
    );
    assert_eq!(
        other.verifying_key().verify(b"message", &signature),
        Err(Error::Verification)
    );
    assert_eq!(
        other.verifying_key().verify_strict(b"message", &signature),
        Err(Error::Verification)
    );
}
