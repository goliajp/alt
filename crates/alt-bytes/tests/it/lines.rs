use alt_bytes::ByteSlice;

fn lines(bytes: &[u8]) -> Vec<&[u8]> {
    bytes.lines().collect()
}

#[test]
fn empty_input_has_no_lines() {
    assert!(lines(b"").is_empty());
}

#[test]
fn a_final_terminator_does_not_start_another_line() {
    assert_eq!(lines(b"a\nb\n"), [&b"a"[..], b"b"]);
    assert_eq!(lines(b"a\nb"), [&b"a"[..], b"b"]);
    assert_eq!(lines(b"a"), [b"a"]);
}

#[test]
fn blank_lines_are_kept() {
    assert_eq!(lines(b"\n"), [b""]);
    assert_eq!(lines(b"\n\n"), [b"", b""]);
    assert_eq!(lines(b"a\n\nb"), [&b"a"[..], b"", b"b"]);
}

#[test]
fn crlf_is_one_terminator_and_only_one_cr_is_removed() {
    assert_eq!(lines(b"a\r\nb\r\n"), [&b"a"[..], b"b"]);
    assert_eq!(lines(b"a\r\r\n"), [b"a\r"]);
    assert_eq!(lines(b"\r\n"), [b""]);
}

#[test]
fn a_bare_cr_is_not_a_terminator() {
    assert_eq!(lines(b"a\rb\n"), [b"a\rb"]);
    assert_eq!(lines(b"a\r"), [b"a\r"]);
}

#[test]
fn lines_are_split_on_bytes_regardless_of_encoding() {
    assert_eq!(lines(b"\xff\n\xfe"), [b"\xff", b"\xfe"]);
}
