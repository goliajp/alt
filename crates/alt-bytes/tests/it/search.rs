use alt_bytes::ByteSlice;

#[test]
fn byte_search_on_empty_input_finds_nothing() {
    assert_eq!(b"".find_byte(0), None);
    assert_eq!(b"".rfind_byte(0), None);
}

#[test]
fn byte_search_reports_first_and_last_occurrence() {
    let text = b"100644 path with spaces\0rest";
    assert_eq!(text.find_byte(b' '), Some(6));
    assert_eq!(text.rfind_byte(b' '), Some(16));
    assert_eq!(text.find_byte(0), Some(23));
    assert_eq!(text.find_byte(b'1'), Some(0));
    assert_eq!(text.rfind_byte(b't'), Some(text.len() - 1));
    assert_eq!(text.find_byte(b'z'), None);
    assert_eq!(text.rfind_byte(b'z'), None);
}

#[test]
fn byte_search_agrees_with_a_linear_scan_at_every_position() {
    // lengths on both sides of the eight-byte word the search reads at a
    // time, for needles with and without the high bit set
    for needle in [0x00, 0x01, 0x7f, 0x80, 0xff, b'a'] {
        // one below the needle, where a borrow could leak into the next byte
        let filler = needle.wrapping_sub(1);
        for len in 0..40 {
            for at in 0..len {
                let mut bytes = vec![filler; len];
                bytes[at] = needle;
                assert_eq!(bytes.find_byte(needle), Some(at), "{needle} {len} {at}");
                assert_eq!(bytes.rfind_byte(needle), Some(at), "{needle} {len} {at}");
                bytes[len - 1] = needle;
                assert_eq!(bytes.find_byte(needle), Some(at));
                assert_eq!(bytes.rfind_byte(needle), Some(len - 1));
                bytes[0] = needle;
                assert_eq!(bytes.find_byte(needle), Some(0));
            }
            assert_eq!(vec![filler; len].find_byte(needle), None);
            assert_eq!(vec![filler; len].rfind_byte(needle), None);
        }
    }
}

#[test]
fn byte_search_does_not_mistake_neighbouring_values() {
    // 0x00 followed by 0x01 is the pattern a borrow-based word test
    // misreports
    assert_eq!(
        b"\x01\x01\x01\x01\x01\x01\x01\x01\x00".find_byte(0),
        Some(8)
    );
    assert_eq!(b"\x00\x01\x01\x01\x01\x01\x01\x01".rfind_byte(0), Some(0));
    assert_eq!(b"\x00\x01\x00\x00\x00\x00\x00\x00".rfind_byte(1), Some(1));
    assert_eq!(b"\x81\x81\x81\x81\x81\x81\x81\x81".find_byte(0x80), None);
}

#[test]
fn substring_search_finds_the_first_match() {
    let commit = b"tree abc\nauthor a\n\nsubject\n\nbody";
    assert_eq!(commit.find(b"\n\n"), Some(17));
    assert_eq!(commit.find("subject"), Some(19));
    assert_eq!(commit.find(b"tree"), Some(0));
    assert_eq!(commit.find(b"body"), Some(commit.len() - 4));
    assert_eq!(commit.find(b"\n\n\n"), None);
}

#[test]
fn substring_search_handles_empty_and_oversized_needles() {
    assert_eq!(b"abc".find(b""), Some(0));
    assert_eq!(b"".find(b""), Some(0));
    assert_eq!(b"".find(b"a"), None);
    assert_eq!(b"ab".find(b"abc"), None);
    assert_eq!(b"abc".find(b"abc"), Some(0));
}

#[test]
fn substring_search_resumes_after_a_partial_match() {
    assert_eq!(b"aaab".find(b"aab"), Some(1));
    assert_eq!(b"\n\r\n\n".find(b"\n\n"), Some(2));
    assert_eq!(b"ababac".find(b"abac"), Some(2));
    assert_eq!(b"aaaa".find(b"ab"), None);
    assert_eq!(b"xa".find(b"ab"), None);
}
