use alt_bytes::{BString, ByteSlice};

fn debug(bytes: &[u8]) -> String {
    format!("{:?}", bytes.as_bstr())
}

fn display(bytes: &[u8]) -> String {
    bytes.as_bstr().to_string()
}

#[test]
fn debug_quotes_plain_text() {
    assert_eq!(debug(b""), r#""""#);
    assert_eq!(debug(b"refs/heads/main"), r#""refs/heads/main""#);
    assert_eq!(debug("naïve 日本語 🦀".as_bytes()), r#""naïve 日本語 🦀""#);
}

#[test]
fn debug_escapes_quotes_and_backslash() {
    assert_eq!(debug(br#"say "hi""#), r#""say \"hi\"""#);
    assert_eq!(debug(b"it's"), r#""it\'s""#);
    assert_eq!(debug(br"a\b"), r#""a\\b""#);
}

#[test]
fn debug_escapes_control_characters() {
    assert_eq!(debug(b"a\tb\r\nc"), r#""a\tb\r\nc""#);
    assert_eq!(debug(b"\0"), r#""\0""#);
    assert_eq!(debug(b"\x01\x1b\x7f"), r#""\x01\x1b\x7f""#);
    // beyond ascii the escapes are those of `str`
    assert_eq!(debug("\u{85}\u{200b}".as_bytes()), r#""\u{85}\u{200b}""#);
    assert_eq!(debug("e\u{301}".as_bytes()), r#""e\u{301}""#);
}

#[test]
fn debug_shows_invalid_utf8_as_hex_escapes() {
    assert_eq!(debug(b"\xff"), r#""\xff""#);
    assert_eq!(debug(b"a\xffb\xfe"), r#""a\xffb\xfe""#);
    // a truncated three-byte sequence and a lone continuation byte
    assert_eq!(debug(b"\xe2\x82"), r#""\xe2\x82""#);
    assert_eq!(debug(b"ok\x80"), r#""ok\x80""#);
    // an overlong encoding is invalid byte by byte
    assert_eq!(debug(b"\xc0\xaf"), r#""\xc0\xaf""#);
}

#[test]
fn debug_keeps_a_real_replacement_character_apart_from_invalid_bytes() {
    assert_eq!(debug("\u{fffd}".as_bytes()), "\"\u{fffd}\"");
    assert_eq!(debug(b"\xef\xbf\xbd\xff"), "\"\u{fffd}\\xff\"");
}

#[test]
fn display_writes_valid_utf8_verbatim() {
    assert_eq!(display(b""), "");
    assert_eq!(display(b"a \"b\"\n\t\0"), "a \"b\"\n\t\0");
    assert_eq!(display("日本語".as_bytes()), "日本語");
}

#[test]
fn display_replaces_each_maximal_invalid_sequence_once() {
    assert_eq!(display(b"a\xffb"), "a\u{fffd}b");
    assert_eq!(display(b"\xff\xfe"), "\u{fffd}\u{fffd}");
    // the two bytes are the prefix of one character, so they are one unit
    assert_eq!(display(b"\xe2\x82z"), "\u{fffd}z");
    assert_eq!(display(b"\xf0\x9f\xa6"), "\u{fffd}");
}

#[test]
fn display_pads_only_under_an_explicit_alignment() {
    let name = b"ab".as_bstr();
    assert_eq!(format!("[{name:5}]"), "[ab]");
    assert_eq!(format!("[{name:<5}]"), "[ab   ]");
    assert_eq!(format!("[{name:>5}]"), "[   ab]");
    assert_eq!(format!("[{name:^5}]"), "[ ab  ]");
    assert_eq!(format!("[{name:*^6}]"), "[**ab**]");
    assert_eq!(format!("[{name:<1}]"), "[ab]");
}

#[test]
fn display_width_counts_characters_and_ignores_precision() {
    let wide = "日本".as_bytes().as_bstr();
    assert_eq!(format!("[{wide:>4}]"), "[  日本]");
    let broken = b"a\xe2\x82".as_bstr();
    assert_eq!(format!("[{broken:<4}]"), "[a\u{fffd}  ]");
    let long = b"abcdef".as_bstr();
    assert_eq!(format!("{long:.2}"), "abcdef");
}

#[test]
fn owned_strings_format_like_borrowed_ones() {
    let owned = BString::from(&b"tab\there \xff 'q'"[..]);
    assert_eq!(format!("{owned:?}"), r#""tab\there \xff \'q\'""#);
    assert_eq!(format!("{owned}"), "tab\there \u{fffd} 'q'");
    assert_eq!(format!("[{owned:>20}]"), "[      tab\there \u{fffd} 'q']");
    assert_eq!(
        format!("{:?}", Some(&owned)),
        r#"Some("tab\there \xff \'q\'")"#
    );
}
