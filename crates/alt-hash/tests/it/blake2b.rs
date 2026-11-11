use super::{hex, noise};
use alt_hash::Blake2b;

fn blake2b(out_len: usize, data: &[u8]) -> Vec<u8> {
    let mut hasher = Blake2b::new(out_len);
    hasher.update(data);
    let mut out = vec![0; out_len];
    hasher.finalize(&mut out);
    out
}

#[test]
fn rfc_7693_vector() {
    assert_eq!(
        hex(&blake2b(64, b"abc")),
        "ba80a53f981c4d0d6a2797b69f12f6e94c212f14685ac4b74b12bb6fdbffa2d17d87c5392aab792dc252d5de4533cc9518d38aa8dbf1925ab92386edd4009923"
    );
    assert_eq!(
        hex(&blake2b(64, b"")),
        "786a02f742015903c6c6fd852552d272912f4740e15847618a86e217f71f5419d25e1031afee585313896444934eb04b903a685b1448b755d56f701afe9be2ce"
    );
}

/// (input length, digest length, digest) over a prefix of `noise(1100, 0x9e37_79b9_7f4a_7c15)`
static BOUNDARIES: [(usize, usize, &str); 27] = [
    (0, 1, "2e"),
    (
        0,
        32,
        "0e5751c026e543b2e8ab2eb06099daa1d1e5df47778f7787faab45cdf12fe3a8",
    ),
    (
        0,
        64,
        "786a02f742015903c6c6fd852552d272912f4740e15847618a86e217f71f5419d25e1031afee585313896444934eb04b903a685b1448b755d56f701afe9be2ce",
    ),
    (1, 1, "85"),
    (
        1,
        32,
        "2a74e3be3c66c735c3de41de6ac2c1872a63efb224bb9826e76e63ac4d0ba9d5",
    ),
    (
        1,
        64,
        "4a38c874192d7a27626835c6c928bd8aab005d3ef726f92dbaee4974656d30c5b19157d851f8c96a4c50f0bdac862bb89045edb46d0e9684425a39ba5880863f",
    ),
    (127, 1, "17"),
    (
        127,
        32,
        "364203a29c95556a0cf3bda6ce3053d4ee434378ac55ca35bd31302594806d02",
    ),
    (
        127,
        64,
        "4ca6ada1be242f52ee68961d4bec86fb4cb35908c3b66ac4c25d5cf1c1599e653b801e8ce14b6b4d41adc6523cd519d909a907362f8ff81438af34fcde041934",
    ),
    (128, 1, "57"),
    (
        128,
        32,
        "9fe29031bfdaa29d0baad731bcf14f03f167a1c8a768d6862949fbd66850d358",
    ),
    (
        128,
        64,
        "da2f6bedeaaa6746e90cef3d4bf944246b41de75a4217d216312c5258911f8743330d764f4eb973cc5bbc6c909f620ecf9642a2065efb366739eebc40febf289",
    ),
    (129, 1, "0b"),
    (
        129,
        32,
        "606d992ece3e919ec4d4ab4a2555e2050934e276bd2e9bff984f1742b8cd8533",
    ),
    (
        129,
        64,
        "5a47c8fa91f0e3cbfa7254f8181f949abda99c87fa3ca5761bc605ea06c3ddd7b2919c1f0699546887925fb648d280d4404298a65249bb28c77cfd66bf2371bd",
    ),
    (255, 1, "82"),
    (
        255,
        32,
        "2b67d666bda7fa3d26a7925c2dc264b7c8b10b6effbc320b05f441b58751aad5",
    ),
    (
        255,
        64,
        "2753a1fa3a6e3a938ff9ee96896c7ec8d24865c4a51bcbfb0c2b8678b6dfe986bfbc57bb0a156c13cc0e60a252c0383d3949fc1be47d9e2cf113f4eb17e4ae41",
    ),
    (256, 1, "d0"),
    (
        256,
        32,
        "55e1a72ea2d47db3478a05c6b0092deaaf47052a96da318af80473f2b30f06fe",
    ),
    (
        256,
        64,
        "b634afb94db26496f7d083245078e2daf31d160deb1e18fb81be13d8249802eb7554eaf3d3932f99a1c9859a96dbd6e885584929065b77fb37fb86100255ff9a",
    ),
    (257, 1, "b9"),
    (
        257,
        32,
        "925b136d5b539263fa958de52d60ed0397f613c02a115c24488e6f22bbcbb32e",
    ),
    (
        257,
        64,
        "f01867682495e6c2e403bba2f7420b95cc9a79e0c9478b0311dedf2543317f0b980a92369868a3882ba779a5fe12bb75949431caf40196e90ae043d52014d269",
    ),
    (1000, 1, "24"),
    (
        1000,
        32,
        "0caa5f1a87272321c1fb8cab95455ac1bb291ff87c533726ba5a4bef0b153e7a",
    ),
    (
        1000,
        64,
        "67c63a6dbcda6d8dc9408dfaf04a5ee1a943bbb0f29cd1d1fab6d0033f6a8952e9ad57cb2cdc1ed9b34a2fcbd8c0be038c1626d433ded218b3db50e5e374c5cf",
    ),
];

#[test]
fn block_boundaries_and_digest_lengths() {
    let data = noise(1100, 0x9e37_79b9_7f4a_7c15);
    for (len, out_len, expected) in BOUNDARIES {
        assert_eq!(
            hex(&blake2b(out_len, &data[..len])),
            expected,
            "length {len}, digest {out_len}"
        );
    }
}

#[test]
fn streaming_splits_do_not_matter() {
    let data = noise(3000, 7);
    for out_len in [1, 20, 64] {
        let whole = blake2b(out_len, &data);
        for step in [1, 3, 127, 128, 129, 256, 1000] {
            let mut hasher = Blake2b::new(out_len);
            for chunk in data.chunks(step) {
                hasher.update(chunk);
            }
            let mut out = vec![0; out_len];
            hasher.finalize(&mut out);
            assert_eq!(out, whole, "digest {out_len}, step {step}");
        }
    }
}

#[test]
#[should_panic(expected = "blake2b digest length")]
fn rejects_a_digest_longer_than_64_bytes() {
    Blake2b::new(65);
}

#[test]
fn accelerated_compression_matches_portable() {
    use alt_hash::blake2b::{self, IV};
    println!("blake2b: {}", blake2b::implementation());
    for case in 1..200u64 {
        let block: [u8; 128] = if case == 1 {
            [0xff; 128]
        } else {
            noise(128, case).try_into().unwrap()
        };
        let mut fast = IV;
        fast[0] ^= case;
        let mut slow = fast;
        // both halves of the 128-bit length, and both values of the final-block flag
        let bytes = (u128::from(case) << 60) | u128::from(case * 131);
        let last = case % 3 == 0;
        // two blocks in a row, so the second starts from a state with every bit in play
        for _ in 0..2 {
            blake2b::compress(&mut fast, &block, bytes, last);
            blake2b::portable(&mut slow, &block, bytes, last);
        }
        assert_eq!(fast, slow, "case {case}");
    }
}
