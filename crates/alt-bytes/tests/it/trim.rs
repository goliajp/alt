use alt_bytes::ByteSlice;

#[test]
fn trim_removes_whitespace_from_both_ends_only() {
    assert_eq!(b"  a b \t\r\n".trim(), b"a b");
    assert_eq!(b"a".trim(), b"a");
    assert_eq!(b"".trim(), b"");
    assert_eq!(b" \n\t ".trim(), b"");
}

#[test]
fn trim_end_leaves_the_start_alone() {
    assert_eq!(b"  a b \t\r\n".trim_end(), b"  a b");
    assert_eq!(b"".trim_end(), b"");
    assert_eq!(b"\n\n".trim_end(), b"");
    assert_eq!(b" a".trim_end(), b" a");
}

#[test]
fn trim_recognises_unicode_whitespace() {
    // no-break space, ideographic space, next line
    let padded = "\u{a0}\u{3000}x\u{85}\u{2003}".as_bytes();
    assert_eq!(padded.trim(), b"x");
    assert_eq!(padded.trim_end(), "\u{a0}\u{3000}x".as_bytes());
    // zero width space is not whitespace
    assert_eq!("\u{200b}".as_bytes().trim(), "\u{200b}".as_bytes());
}

#[test]
fn trim_keeps_non_whitespace_multibyte_characters() {
    assert_eq!(" é ".as_bytes().trim(), "é".as_bytes());
    assert_eq!("日本 ".as_bytes().trim_end(), "日本".as_bytes());
}

#[test]
fn trim_stops_at_invalid_utf8() {
    assert_eq!(b" \xff ".trim(), b"\xff");
    assert_eq!(b"\xff \xff".trim(), b"\xff \xff");
    // 0xa0 alone is a stray continuation byte, not a no-break space
    assert_eq!(b"a \xa0".trim_end(), b"a \xa0");
    assert_eq!(b"\xa0 a".trim(), b"\xa0 a");
    // 0xc2 before a space is a truncated sequence
    assert_eq!(b"a\xc2 ".trim_end(), b"a\xc2");
    assert_eq!(b" \xc2".trim(), b"\xc2");
}
