use alt_base64::{DecodeError, Engine, STANDARD, URL_SAFE_NO_PAD};

fn bytes(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(167).wrapping_add(seed))
        .collect()
}

fn round_trips(engine: &Engine) {
    for len in 0..=260 {
        for seed in [0, 1, 0x7f, 0xff] {
            let input = bytes(len, seed);
            assert_eq!(engine.decode(engine.encode(&input)), Ok(input));
        }
    }
}

#[test]
fn every_length_round_trips() {
    round_trips(&STANDARD);
    round_trips(&URL_SAFE_NO_PAD);
}

#[test]
fn every_byte_value_round_trips() {
    let input: Vec<u8> = (0..=255).collect();
    assert_eq!(STANDARD.decode(STANDARD.encode(&input)), Ok(input.clone()));
    assert_eq!(
        URL_SAFE_NO_PAD.decode(URL_SAFE_NO_PAD.encode(&input)),
        Ok(input)
    );
}

#[test]
fn standard_decodes_rfc4648_vectors() {
    for (encoded, plain) in [
        ("", ""),
        ("Zg==", "f"),
        ("Zm8=", "fo"),
        ("Zm9v", "foo"),
        ("Zm9vYg==", "foob"),
        ("Zm9vYmE=", "fooba"),
        ("Zm9vYmFy", "foobar"),
    ] {
        assert_eq!(STANDARD.decode(encoded), Ok(plain.as_bytes().to_vec()));
    }
}

#[test]
fn bytes_outside_the_alphabet_are_rejected() {
    for input in [
        "Zm9\n",
        "Zm 9",
        " Zm9",
        "Zm\r\nYmFy",
        "Zm9\0",
        "Zmé",
        "Zm-_",
        "Z.9v",
        "Zm9v!A==",
    ] {
        assert_eq!(
            STANDARD.decode(input),
            Err(DecodeError::InvalidSymbol),
            "{input:?}"
        );
    }
    for input in ["Zm+/", "Zm9 ", "Zm\n9", "Z~"] {
        assert_eq!(
            URL_SAFE_NO_PAD.decode(input),
            Err(DecodeError::InvalidSymbol),
            "{input:?}"
        );
    }
    for byte in 0..=255u8 {
        let in_alphabet = byte.is_ascii_alphanumeric() || byte == b'+' || byte == b'/';
        let input = [b'A', byte, b'A', b'A'];
        assert_eq!(STANDARD.decode(input).is_ok(), in_alphabet, "{byte:#04x}");
    }
}

#[test]
fn standard_requires_exact_padding() {
    for input in ["Zg", "Zg=", "Zm8", "Z", "Zm9vYg", "Zm9vYmE", "Zm9vY", "="] {
        assert_eq!(
            STANDARD.decode(input),
            Err(DecodeError::InvalidPadding),
            "{input:?}"
        );
    }
}

#[test]
fn misplaced_padding_is_rejected() {
    for input in [
        "====",
        "Z===",
        "=Zg=",
        "Zg=A",
        "Z=g=",
        "Zg==Zg==",
        "Zm8=Zm9v",
        "Zm9v====",
        "Zm9vZg===",
        "==Zg",
    ] {
        assert!(STANDARD.decode(input).is_err(), "{input:?}");
    }
}

#[test]
fn url_safe_no_pad_forbids_padding() {
    for input in ["Zg==", "Zm8=", "Zg=", "Zm9vZg=", "=="] {
        assert_eq!(
            URL_SAFE_NO_PAD.decode(input),
            Err(DecodeError::InvalidSymbol),
            "{input:?}"
        );
    }
    assert_eq!(URL_SAFE_NO_PAD.decode("Zg"), Ok(b"f".to_vec()));
    assert_eq!(URL_SAFE_NO_PAD.decode("Zm8"), Ok(b"fo".to_vec()));
}

#[test]
fn a_single_dangling_symbol_is_rejected() {
    for input in ["Z", "Zm9vY", "Zm9vYmFyZ"] {
        assert_eq!(
            URL_SAFE_NO_PAD.decode(input),
            Err(DecodeError::InvalidLength),
            "{input:?}"
        );
    }
    assert_eq!(STANDARD.decode("Z==="), Err(DecodeError::InvalidSymbol));
}

#[test]
fn non_canonical_trailing_bits_are_rejected() {
    // "Zg==" is the only two-symbol spelling of "f": each of the other 15
    // values of the low four bits must be refused
    for last in "hijklmnopqrstuv".chars() {
        let padded = format!("Z{last}==");
        assert_eq!(STANDARD.decode(&padded), Err(DecodeError::TrailingBits));
        let bare = format!("Z{last}");
        assert_eq!(
            URL_SAFE_NO_PAD.decode(&bare),
            Err(DecodeError::TrailingBits)
        );
    }
    for last in ["9", "-", "_"] {
        let bare = format!("Zm{last}");
        assert_eq!(
            URL_SAFE_NO_PAD.decode(&bare),
            Err(DecodeError::TrailingBits)
        );
    }
    for input in ["Zm9=", "Zm+=", "Zm/=", "Zm9vYmF="] {
        assert_eq!(STANDARD.decode(input), Err(DecodeError::TrailingBits));
    }
    assert_eq!(URL_SAFE_NO_PAD.decode("Zm8"), Ok(b"fo".to_vec()));
}

#[test]
fn decode_error_messages_are_distinct() {
    let messages = [
        DecodeError::InvalidSymbol,
        DecodeError::InvalidLength,
        DecodeError::InvalidPadding,
        DecodeError::TrailingBits,
    ]
    .map(|error| error.to_string());
    for (i, message) in messages.iter().enumerate() {
        assert!(!messages[i + 1..].contains(message));
    }
}
