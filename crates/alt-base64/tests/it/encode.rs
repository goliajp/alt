use alt_base64::{STANDARD, STANDARD_NO_PAD, URL_SAFE_NO_PAD};

const RFC4648: [(&str, &str); 7] = [
    ("", ""),
    ("f", "Zg=="),
    ("fo", "Zm8="),
    ("foo", "Zm9v"),
    ("foob", "Zm9vYg=="),
    ("fooba", "Zm9vYmE="),
    ("foobar", "Zm9vYmFy"),
];

#[test]
fn standard_matches_rfc4648_vectors() {
    for (plain, encoded) in RFC4648 {
        assert_eq!(STANDARD.encode(plain), encoded);
    }
}

#[test]
fn url_safe_no_pad_matches_rfc4648_vectors_without_padding() {
    for (plain, encoded) in RFC4648 {
        assert_eq!(URL_SAFE_NO_PAD.encode(plain), encoded.trim_end_matches('='));
        assert_eq!(STANDARD_NO_PAD.encode(plain), encoded.trim_end_matches('='));
    }
}

#[test]
fn alphabets_differ_only_in_the_last_two_symbols() {
    let every_symbol: Vec<u8> = (0..64u32)
        .flat_map(|v| {
            let n = v << 18 | v << 12 | v << 6 | v;
            n.to_be_bytes()[1..].to_vec()
        })
        .collect();
    let standard = STANDARD.encode(&every_symbol);
    let url_safe = URL_SAFE_NO_PAD.encode(&every_symbol);
    assert!(standard.starts_with("AAAABBBB"));
    assert!(standard.ends_with("9999++++////"));
    assert!(url_safe.ends_with("9999----____"));
    assert_eq!(standard.replace('+', "-").replace('/', "_"), url_safe);
}

#[test]
fn high_bytes_use_the_top_of_the_alphabet() {
    assert_eq!(STANDARD.encode([0xfb, 0xff]), "+/8=");
    assert_eq!(STANDARD_NO_PAD.encode([0xfb, 0xff]), "+/8");
    assert_eq!(URL_SAFE_NO_PAD.encode([0xfb, 0xff]), "-_8");
    assert_eq!(STANDARD.encode([0xff; 3]), "////");
}

#[test]
fn encoded_length_follows_the_padding_rule() {
    for len in 0..64usize {
        let input = vec![0xa5; len];
        assert_eq!(STANDARD.encode(&input).len(), len.div_ceil(3) * 4);
        assert_eq!(URL_SAFE_NO_PAD.encode(&input).len(), (len * 4).div_ceil(3));
    }
}
