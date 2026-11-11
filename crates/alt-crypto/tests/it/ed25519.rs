mod edge;
mod edge_cases;
mod vectors;

use super::{array, hex, noise, unhex};
use alt_crypto::ed25519::{Error, Signature, SigningKey, VerifyingKey};
use ed25519_dalek::{Signer, Verifier};

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

#[test]
fn signatures_and_public_keys_match_dalek() {
    for case in 0..1500u64 {
        let seed: [u8; 32] = array(&noise(32, case * 2 + 1));
        let message = noise((case as usize * 7) % 400, case * 2 + 2);
        let ours = SigningKey::from_bytes(&seed);
        let theirs = ed25519_dalek::SigningKey::from_bytes(&seed);
        assert_eq!(ours.to_bytes(), theirs.to_bytes());
        assert_eq!(
            ours.verifying_key().to_bytes(),
            theirs.verifying_key().to_bytes(),
            "public key, case {case}"
        );
        let signature = ours.sign(&message);
        assert_eq!(
            signature.to_bytes(),
            theirs.sign(&message).to_bytes(),
            "signature, case {case}"
        );
        // each side accepts the other's output
        let dalek_signature = ed25519_dalek::Signature::from_bytes(&signature.to_bytes());
        assert!(
            theirs
                .verifying_key()
                .verify(&message, &dalek_signature)
                .is_ok()
        );
        assert!(
            theirs
                .verifying_key()
                .verify_strict(&message, &dalek_signature)
                .is_ok()
        );
        assert_eq!(ours.verifying_key().verify(&message, &signature), Ok(()));
        assert_eq!(
            ours.verifying_key().verify_strict(&message, &signature),
            Ok(())
        );
    }
}

#[test]
fn extreme_seeds_match_dalek() {
    for seed in [[0u8; 32], [0xff; 32], [0x80; 32], [1; 32]] {
        let ours = SigningKey::from_bytes(&seed);
        let theirs = ed25519_dalek::SigningKey::from_bytes(&seed);
        assert_eq!(
            ours.verifying_key().to_bytes(),
            theirs.verifying_key().to_bytes()
        );
        for message in [&b""[..], b"a", &[0xff; 200]] {
            assert_eq!(
                ours.sign(message).to_bytes(),
                theirs.sign(message).to_bytes()
            );
        }
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
