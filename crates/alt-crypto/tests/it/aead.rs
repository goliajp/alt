use super::{array, hex, noise, unhex};
use alt_crypto::aead::{self, chacha20, poly1305::Poly1305};
use chacha20poly1305::{
    ChaCha20Poly1305, XChaCha20Poly1305,
    aead::{Aead, KeyInit, Payload},
};

const SUNSCREEN: &[u8] = b"Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the future, sunscreen would be it.";

fn counting(from: u8) -> [u8; 32] {
    std::array::from_fn(|i| from + i as u8)
}

#[test]
fn rfc_8439_chacha20_block() {
    let nonce = array(&unhex("000000090000004a00000000"));
    assert_eq!(
        hex(&chacha20::block(&counting(0), &nonce, 1)),
        "10f1e7e4d13b5915500fdd1fa32071c4c7d1f4c733c068030422aa9ac3d46c4e\
         d2826446079faa0914c2d705d98b02a2b5129cd1de164eb9cbd083e8a2503c4e"
    );
}

#[test]
fn rfc_8439_chacha20_encryption() {
    let nonce = array(&unhex("000000000000004a00000000"));
    let mut text = SUNSCREEN.to_vec();
    chacha20::apply(&counting(0), &nonce, 1, &mut text);
    assert_eq!(
        hex(&text),
        "6e2e359a2568f98041ba0728dd0d6981e97e7aec1d4360c20a27afccfd9fae0b\
         f91b65c5524733ab8f593dabcd62b3571639d624e65152ab8f530c359f0861d8\
         07ca0dbf500d6a6156a38e088a22b65e52bc514d16ccf806818ce91ab7793736\
         5af90bbf74a35be6b40b8eedf2785e42874d"
    );
}

#[test]
fn rfc_8439_poly1305() {
    let key = array(&unhex(
        "85d6be7857556d337f4452fe42d506a80103808afb0db2fd4abff6af4149f51b",
    ));
    let mut mac = Poly1305::new(&key);
    mac.update(b"Cryptographic Forum Research Group");
    assert_eq!(hex(&mac.finalize()), "a8061dc1305136c6c22b8baf0c0127a9");
}

#[test]
fn poly1305_reduces_an_accumulator_at_the_modulus() {
    // with r = 1 two blocks m1, m2 leave h = m1 + m2 + 2^129; m1 = 2^128 - 1 and
    // m2 = 2^128 - 1 - d put h at 2^130 - 2 - d, straddling the modulus 2^130 - 5
    let mut key = [0u8; 32];
    key[0] = 1;
    for (d, expected) in [
        (0u8, 3u128),
        (1, 2),
        (2, 1),
        (3, 0),
        (4, u128::MAX - 5),
        (5, u128::MAX - 6),
    ] {
        let mut message = [0xffu8; 32];
        message[16] -= d;
        let mut mac = Poly1305::new(&key);
        mac.update(&message);
        assert_eq!(mac.finalize(), expected.to_le_bytes(), "d = {d}");
    }
}

#[test]
fn rfc_8439_aead() {
    let nonce = array(&unhex("070000004041424344454647"));
    let aad = unhex("50515253c0c1c2c3c4c5c6c7");
    let sealed = aead::encrypt_ietf(&counting(0x80), &nonce, &aad, SUNSCREEN);
    assert_eq!(
        hex(&sealed),
        "d31a8d34648e60db7b86afbc53ef7ec2a4aded51296e08fea9e2b5a736ee62d6\
         3dbea45e8ca9671282fafb69da92728b1a71de0a9e060b2905d6a5b67ecd3b36\
         92ddbd7f2d778b8c9803aee328091b58fab324e4fad675945585808b4831d7bc\
         3ff4def08e4b7a9de576d26586cec64b6116\
         1ae10b594f09e26a7e902ecbd0600691"
    );
    assert_eq!(
        aead::decrypt_ietf(&counting(0x80), &nonce, &aad, &sealed).unwrap(),
        SUNSCREEN
    );
}

#[test]
fn xchacha_draft_hchacha20() {
    let nonce = array(&unhex("000000090000004a0000000031415927"));
    assert_eq!(
        hex(&chacha20::hchacha20(&counting(0), &nonce)),
        "82413b4227b27bfed30e42508a877d73a0f9e4d58a74a853c12ec41326d3ecdc"
    );
}

#[test]
fn xchacha_draft_aead() {
    let nonce = array(&unhex("404142434445464748494a4b4c4d4e4f5051525354555657"));
    let aad = unhex("50515253c0c1c2c3c4c5c6c7");
    let sealed = aead::encrypt(&counting(0x80), &nonce, &aad, SUNSCREEN);
    assert_eq!(
        hex(&sealed),
        "bd6d179d3e83d43b9576579493c0e939572a1700252bfaccbed2902c21396cbb\
         731c7f1b0b4aa6440bf3a82f4eda7e39ae64c6708c54c216cb96b72e1213b452\
         2f8c9ba40db5d945b11b69b982c1bb9e3f3fac2bc369488f76b2383565d3fff9\
         21f9664c97637da9768812f615c68b13b52e\
         c0875924c1c7987947deafd8780acf49"
    );
    assert_eq!(
        aead::decrypt(&counting(0x80), &nonce, &aad, &sealed).unwrap(),
        SUNSCREEN
    );
}

#[test]
fn accelerated_chacha20_matches_portable() {
    println!("chacha20: {}", chacha20::implementation());
    let key = array(&noise(32, 3));
    let nonce = array(&noise(12, 5));
    let data = noise(2500, 11);
    for len in (0..data.len())
        .step_by(7)
        .chain([255, 256, 257, 511, 512, 513, 1024, 2048])
    {
        // counters next to the 32-bit wrap check the per-block counter arithmetic
        for counter in [0, 1, u32::MAX - 5, u32::MAX] {
            let mut fast = data[..len].to_vec();
            let mut slow = fast.clone();
            chacha20::apply(&key, &nonce, counter, &mut fast);
            chacha20::apply_portable(&key, &nonce, counter, &mut slow);
            assert_eq!(fast, slow, "length {len}, counter {counter}");
        }
    }
}

#[test]
fn matches_chacha20poly1305_crate_at_every_length() {
    let key: [u8; 32] = array(&noise(32, 17));
    let reference = XChaCha20Poly1305::new((&key).into());
    let short = ChaCha20Poly1305::new((&key).into());
    let data = noise(1000, 19);
    for len in 0..=1000 {
        let nonce: [u8; 24] = array(&noise(24, 23 + len as u64));
        let aad_len = [0, 0, 1, 12, 16, 17, 100][len % 7];
        let aad = noise(aad_len, 29 + len as u64);
        let msg = &data[..len];
        let ours = aead::encrypt(&key, &nonce, &aad, msg);
        let theirs = reference
            .encrypt((&nonce).into(), Payload { msg, aad: &aad })
            .unwrap();
        assert_eq!(ours, theirs, "length {len}");
        assert_eq!(aead::decrypt(&key, &nonce, &aad, &theirs).unwrap(), msg);
        assert_eq!(
            reference
                .decrypt(
                    (&nonce).into(),
                    Payload {
                        msg: &ours,
                        aad: &aad
                    }
                )
                .unwrap(),
            msg
        );
        let nonce: [u8; 12] = array(&nonce[..12]);
        assert_eq!(
            aead::encrypt_ietf(&key, &nonce, &aad, msg),
            short
                .encrypt((&nonce).into(), Payload { msg, aad: &aad })
                .unwrap(),
            "ietf length {len}"
        );
    }
}

#[test]
fn matches_chacha20poly1305_crate_on_large_messages() {
    let key: [u8; 32] = array(&noise(32, 31));
    let nonce: [u8; 24] = array(&noise(24, 37));
    let reference = XChaCha20Poly1305::new((&key).into());
    for len in [4096, 65_536, 65_537, 1 << 20] {
        let msg = noise(len, 41);
        let ours = aead::encrypt(&key, &nonce, b"header", &msg);
        let theirs = reference
            .encrypt(
                (&nonce).into(),
                Payload {
                    msg: &msg,
                    aad: b"header",
                },
            )
            .unwrap();
        assert_eq!(ours, theirs, "length {len}");
    }
}

#[test]
fn any_tampering_fails_decryption() {
    let key: [u8; 32] = array(&noise(32, 43));
    let nonce: [u8; 24] = array(&noise(24, 47));
    let aad = b"associated".to_vec();
    for len in [0, 1, 63, 64, 65, 300] {
        let msg = noise(len, 53);
        let sealed = aead::encrypt(&key, &nonce, &aad, &msg);
        assert_eq!(aead::decrypt(&key, &nonce, &aad, &sealed).unwrap(), msg);
        for at in 0..sealed.len() {
            let mut bad = sealed.clone();
            bad[at] ^= 1 << (at % 8);
            assert_eq!(
                aead::decrypt(&key, &nonce, &aad, &bad),
                Err(aead::Error),
                "byte {at}"
            );
        }
        for at in 0..aad.len() {
            let mut bad = aad.clone();
            bad[at] ^= 0x80;
            assert_eq!(aead::decrypt(&key, &nonce, &bad, &sealed), Err(aead::Error));
        }
        assert_eq!(aead::decrypt(&key, &nonce, b"", &sealed), Err(aead::Error));
        for at in 0..24 {
            let mut bad = nonce;
            bad[at] ^= 1;
            assert_eq!(
                aead::decrypt(&key, &bad, &aad, &sealed),
                Err(aead::Error),
                "nonce {at}"
            );
        }
        for at in 0..32 {
            let mut bad = key;
            bad[at] ^= 1;
            assert_eq!(
                aead::decrypt(&bad, &nonce, &aad, &sealed),
                Err(aead::Error),
                "key {at}"
            );
        }
        // truncation, including dropping the whole tag
        for keep in 0..sealed.len() {
            assert_eq!(
                aead::decrypt(&key, &nonce, &aad, &sealed[..keep]),
                Err(aead::Error)
            );
        }
    }
}
