use crate::check::{output, parse, report};
use crate::generate;
use crate::rng::Rng;
use alt_json::Value;

fn quoted(inner: &[u8]) -> Vec<u8> {
    let mut text = vec![b'"'];
    text.extend_from_slice(inner);
    text.push(b'"');
    text
}

/// every scalar value, alone and inside a longer run, written and read back
#[test]
fn every_scalar_value() {
    let mut cases = 0;
    for code in 0..=0x10_ffffu32 {
        let Some(c) = char::from_u32(code) else {
            continue;
        };
        let text = output(&Value::String(c.to_string()));
        assert!(parse(&text));
        let text = output(&Value::String(format!("0123456{c}89abcdef{c}")));
        assert!(parse(&text));
        cases += 2;
    }
    report("strings, every scalar value", cases, None);
}

/// which bytes are escaped, and how, pinned independently of the reference
#[test]
fn escapes_are_pinned() {
    let all: String = (0..0x80u8).map(char::from).collect();
    let expected = concat!(
        r#""\u0000\u0001\u0002\u0003\u0004\u0005\u0006\u0007\b\t\n\u000b\f\r\u000e\u000f"#,
        r#"\u0010\u0011\u0012\u0013\u0014\u0015\u0016\u0017\u0018\u0019\u001a\u001b\u001c\u001d\u001e\u001f"#,
        r##" !\"#$%&'()*+,-./0123456789:;<=>?@ABCDEFGHIJKLMNOPQRSTUVWXYZ[\\]^_`abcdefghijklmnopqrstuvwxyz{|}~"##,
        "\u{7f}\""
    );
    assert_eq!(
        alt_json::to_string(&Value::String(all.clone())).unwrap(),
        expected
    );
    output(&Value::String(all));
    assert_eq!(
        alt_json::to_string(&Value::from("é\u{2028}😀/")).unwrap(),
        "\"é\u{2028}😀/\""
    );
}

#[test]
fn random_strings() {
    let mut rng = Rng::new(0x57e);
    let mut cases = 0;
    for _ in 0..600_000 {
        let s = generate::string(&mut rng, 40);
        let text = output(&Value::String(s.clone()));
        assert!(parse(&text));
        assert_eq!(alt_json::parse(&text).unwrap(), Value::String(s));
        cases += 1;
    }
    report("strings, random strings", cases, None);
}

/// a byte that ends a plain run, at every offset of runs up to 40 bytes
#[test]
fn special_byte_at_every_offset() {
    let (mut cases, mut accepted) = (0, 0);
    let specials = [
        0x00, 0x01, 0x08, 0x0a, 0x1f, 0x20, 0x21, 0x22, 0x23, 0x5b, 0x5c, 0x5d, 0x7f, 0x80, 0xff,
    ];
    for len in 0..=40usize {
        for at in 0..len {
            for special in specials {
                for filler in [b'a', 0x7f, b'#'] {
                    let mut inner = vec![filler; len];
                    inner[at] = special;
                    accepted += u64::from(parse(&quoted(&inner)));
                    cases += 1;
                    if let Ok(s) = String::from_utf8(inner) {
                        output(&Value::String(s));
                    }
                }
            }
        }
    }
    report(
        "strings, special byte at every offset",
        cases,
        Some(accepted),
    );
}

#[test]
fn every_unicode_escape() {
    let (mut cases, mut accepted) = (0, 0);
    for unit in 0..=0xffffu32 {
        for text in [format!("\"\\u{unit:04x}\""), format!("\"a\\u{unit:04X}b\"")] {
            accepted += u64::from(parse(text.as_bytes()));
            cases += 1;
        }
    }
    report("strings, every \\u escape", cases, Some(accepted));
}

#[test]
fn surrogate_pairs() {
    let mut rng = Rng::new(0xd800);
    let (mut cases, mut accepted) = (0, 0);
    for high in 0xd800..=0xdbffu32 {
        for low in [0xdc00, 0xdfff, 0xdc00 + rng.below(0x400) as u32] {
            let text = format!("\"\\u{high:04x}\\u{low:04x}\"");
            accepted += u64::from(parse(text.as_bytes()));
            cases += 1;
        }
    }
    // a high surrogate followed by every possible second unit
    for second in 0..=0xffffu32 {
        let text = format!("\"\\ud83d\\u{second:04x}\"");
        accepted += u64::from(parse(text.as_bytes()));
        cases += 1;
    }
    for tail in [
        "", "\\", "\\u", "\\ud", "\\ude", "\\ude0", "\\n", "x", "\\x", "\\\\", "\\Ude00",
        "\\ud83d", "\\u0041", "\\udg00", "\"", " ", "\\u\"", "\\ude0\"", "\\ude00x",
    ] {
        for text in [format!("\"\\ud83d{tail}\""), format!("\"\\ud83d{tail}")] {
            accepted += u64::from(parse(text.as_bytes()));
            cases += 1;
        }
    }
    report("strings, surrogate pairs", cases, Some(accepted));
}

#[test]
fn every_escape_letter_and_hex_digit() {
    let (mut cases, mut accepted) = (0, 0);
    for byte in 0..=0xffu8 {
        for inner in [
            vec![b'\\', byte],
            vec![b'a', b'\\', byte, b'b'],
            vec![b'\\', b'u', byte, b'0', b'4', b'1'],
            vec![b'\\', b'u', b'0', byte, b'4', b'1'],
            vec![b'\\', b'u', b'0', b'0', byte, b'1'],
            vec![b'\\', b'u', b'0', b'0', b'4', byte],
        ] {
            accepted += u64::from(parse(&quoted(&inner)));
            cases += 1;
            // and with the input ending right after
            let mut cut = vec![b'"'];
            cut.extend_from_slice(&inner);
            accepted += u64::from(parse(&cut));
            cases += 1;
        }
    }
    report(
        "strings, every escape letter and hex digit",
        cases,
        Some(accepted),
    );
}

/// raw bytes: every one- and two-byte content, then random content weighted
/// towards the lead and continuation bytes of utf-8
#[test]
fn raw_bytes_and_invalid_utf8() {
    let mut rng = Rng::new(0xf8);
    let (mut cases, mut accepted) = (0, 0);
    for first in 0..=0xffu8 {
        accepted += u64::from(parse(&quoted(&[first])));
        cases += 1;
        for second in 0..=0xffu8 {
            accepted += u64::from(parse(&quoted(&[first, second])));
            cases += 1;
        }
    }
    let notable = [
        0x00, 0x1f, 0x22, 0x5c, 0x7f, 0x80, 0x8f, 0x90, 0x9f, 0xa0, 0xbf, 0xc0, 0xc1, 0xc2, 0xdf,
        0xe0, 0xe1, 0xec, 0xed, 0xee, 0xef, 0xf0, 0xf1, 0xf3, 0xf4, 0xf5, 0xf7, 0xf8, 0xfe, 0xff,
        b'a', b'u', b'n',
    ];
    for _ in 0..1_500_000 {
        let len = 1 + rng.below(8);
        let inner: Vec<u8> = (0..len)
            .map(|_| {
                if rng.chance(8) {
                    rng.next() as u8
                } else {
                    *rng.pick(&notable)
                }
            })
            .collect();
        accepted += u64::from(parse(&quoted(&inner)));
        cases += 1;
    }
    // escapes next to raw multi-byte text
    for text in [
        "\"é\\u00e9é\"",
        "\"\\u00e9\u{80}\"",
        "\"\u{10ffff}\\ud83d\\ude00\"",
        "\"\\/\\b\\f\\n\\r\\t\\\"\\\\\"",
    ] {
        accepted += u64::from(parse(text.as_bytes()));
        cases += 1;
    }
    for bytes in [
        &b"\"\xc3\\u00a9\""[..],
        b"\"\\u00c3\xa9\"",
        b"\"\xed\xa0\x80\"",
        b"\"\xf4\x90\x80\x80\"",
        b"\"\xc0\xaf\"",
        b"\xef\xbb\xbf\"a\"",
        b"\"\xe2\x82\"",
    ] {
        accepted += u64::from(parse(bytes));
        cases += 1;
    }
    report(
        "strings, raw bytes and invalid utf-8",
        cases,
        Some(accepted),
    );
}
