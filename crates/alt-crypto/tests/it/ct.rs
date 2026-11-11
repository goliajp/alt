use alt_crypto::ct;

#[test]
fn equal_only_for_identical_bytes() {
    assert!(ct::eq(b"", b""));
    assert!(ct::eq(b"abc", b"abc"));
    assert!(!ct::eq(b"abc", b"abd"));
    assert!(!ct::eq(b"abc", b"ab"));
    assert!(!ct::eq(b"", b"a"));
    let base = [0x5au8; 64];
    for at in 0..64 {
        for bit in 0..8 {
            let mut other = base;
            other[at] ^= 1 << bit;
            assert!(!ct::eq(&base, &other), "byte {at} bit {bit}");
        }
    }
}
