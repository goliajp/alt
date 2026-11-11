//! Writes the files under `fixtures/` from the crates this one replaced.
//!
//! Each test first requires the reference crate and this implementation to
//! agree on every case, then records what the reference produced. To
//! regenerate: add `ed25519-dalek = "2"`, `argon2 = "0.5"` and
//! `chacha20poly1305 = "0.10"` to `[dev-dependencies]`, then run
//! `RUSTFLAGS="--cfg alt_crypto_reference" cargo test -p alt-crypto reference -- --ignored`.

use super::{aead, argon2, ed25519};
use alt_crypto::argon2::{PasswordHash, hash_password_into};
use ed25519_dalek::{Signer, Verifier};

fn write(name: &str, bytes: &[u8]) {
    let path = format!("{}/tests/it/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::write(&path, bytes).unwrap();
    println!("{path}: {} bytes", bytes.len());
}

fn dalek(key: &[u8; 32], message: &[u8], signature: &[u8; 64]) -> u8 {
    let Ok(parsed) = ed25519_dalek::VerifyingKey::from_bytes(key) else {
        return 0;
    };
    assert_eq!(parsed.to_bytes(), *key);
    let signature = ed25519_dalek::Signature::from_bytes(signature);
    1 | u8::from(parsed.is_weak()) << 1
        | u8::from(parsed.verify(message, &signature).is_ok()) << 2
        | u8::from(parsed.verify_strict(message, &signature).is_ok()) << 3
}

#[test]
#[ignore = "rewrites fixtures; needs the reference crates"]
fn ed25519_edge() {
    let ours = ed25519::edge::run(ed25519::edge::ours);
    let theirs = ed25519::edge::run(dalek);
    assert_eq!(ours.len(), theirs.len());
    for (index, (ours, theirs)) in ours.iter().zip(&theirs).enumerate() {
        assert_eq!(ours, theirs, "boundary case {index}");
    }
    let outcomes: Vec<u8> = theirs.iter().map(|(_, outcome)| *outcome).collect();
    write("ed25519_edge.bin", &outcomes);
}

#[test]
#[ignore = "rewrites fixtures; needs the reference crates"]
fn ed25519_sign() {
    let mut out = Vec::new();
    for (index, (seed, message)) in ed25519::signing_cases().iter().enumerate() {
        let theirs = ed25519_dalek::SigningKey::from_bytes(seed);
        let ours = alt_crypto::ed25519::SigningKey::from_bytes(seed);
        let public = theirs.verifying_key().to_bytes();
        let signature = theirs.sign(message).to_bytes();
        assert_eq!(ours.verifying_key().to_bytes(), public, "case {index}");
        assert_eq!(ours.sign(message).to_bytes(), signature, "case {index}");
        out.extend_from_slice(&public);
        out.extend_from_slice(&signature);
    }
    write("ed25519_sign.bin", &out);
}

fn argon2id(params: &alt_crypto::argon2::Params, len: usize) -> ::argon2::Argon2<'static> {
    ::argon2::Argon2::new(
        ::argon2::Algorithm::Argon2id,
        ::argon2::Version::V0x13,
        ::argon2::Params::new(params.m_cost, params.t_cost, params.p_cost, Some(len)).unwrap(),
    )
}

#[test]
#[ignore = "rewrites fixtures; needs the reference crates"]
fn argon2_tags() {
    let mut out = Vec::new();
    for (index, case) in argon2::cases().iter().enumerate() {
        let mut theirs = vec![0; case.tag_len];
        argon2id(&case.params, case.tag_len)
            .hash_password_into(&case.password, &case.salt, &mut theirs)
            .unwrap();
        let mut ours = vec![0; case.tag_len];
        hash_password_into(&case.password, &case.salt, &case.params, &mut ours).unwrap();
        assert_eq!(ours, theirs, "case {index}");
        out.extend_from_slice(&theirs);
    }
    write("argon2_tags.bin", &out);
}

#[test]
#[ignore = "rewrites fixtures; needs the reference crates"]
fn argon2_phc() {
    use ::argon2::{
        PasswordHasher, PasswordVerifier,
        password_hash::{PasswordHash as TheirHash, SaltString},
    };
    let mut out = String::new();
    for (index, (params, salt_len)) in argon2::PHC_SAMPLES.into_iter().enumerate() {
        let password = argon2::sample_password(index);
        let salt = super::noise(salt_len, 7000 + index as u64);
        let theirs = argon2id(&params, 32);
        // the way alt-browser-auth called the argon2 crate
        let encoded = theirs
            .hash_password(password.as_bytes(), &SaltString::encode_b64(&salt).unwrap())
            .unwrap()
            .to_string();
        let ours = PasswordHash::new(password.as_bytes(), &salt, &params, 32).unwrap();
        assert_eq!(ours.to_string(), encoded, "line {index}");
        // and the reference accepts a string this implementation wrote under another salt
        let other = super::noise(salt_len, 8000 + index as u64);
        let ours = PasswordHash::new(password.as_bytes(), &other, &params, 32)
            .unwrap()
            .to_string();
        let parsed = TheirHash::new(&ours).unwrap();
        assert!(theirs.verify_password(password.as_bytes(), &parsed).is_ok());
        assert!(theirs.verify_password(b"incorrect", &parsed).is_err());
        out.push_str(&encoded);
        out.push('\n');
    }
    write("argon2_phc.txt", out.as_bytes());
}

#[test]
#[ignore = "rewrites fixtures; needs the reference crates"]
fn aead_digests() {
    use chacha20poly1305::{
        ChaCha20Poly1305, XChaCha20Poly1305,
        aead::{Aead, KeyInit, Payload},
    };
    let mut out = Vec::new();
    for (index, case) in aead::cases().iter().enumerate() {
        let payload = || Payload {
            msg: &case.message,
            aad: &case.aad,
        };
        let long = XChaCha20Poly1305::new((&case.key).into());
        let theirs = long.encrypt((&case.nonce).into(), payload()).unwrap();
        let ours = alt_crypto::aead::encrypt(&case.key, &case.nonce, &case.aad, &case.message);
        assert_eq!(ours, theirs, "case {index}");
        let opened = long.decrypt(
            (&case.nonce).into(),
            Payload {
                msg: &ours,
                aad: &case.aad,
            },
        );
        assert_eq!(opened.unwrap(), case.message, "case {index}");
        out.extend_from_slice(&alt_hash::sha256(&theirs));

        let nonce = aead::short_nonce(case);
        let short = ChaCha20Poly1305::new((&case.key).into());
        let theirs = short.encrypt((&nonce).into(), payload()).unwrap();
        let ours = alt_crypto::aead::encrypt_ietf(&case.key, &nonce, &case.aad, &case.message);
        assert_eq!(ours, theirs, "ietf case {index}");
        out.extend_from_slice(&alt_hash::sha256(&theirs));
    }
    write("aead_digests.bin", &out);
}
