use alt_os::random;

#[test]
fn two_fills_differ() {
    let (mut first, mut second) = ([0u8; 32], [0u8; 32]);
    random::fill(&mut first);
    random::fill(&mut second);
    assert_ne!(first, second);
    assert_ne!(first, [0u8; 32]);
}

#[test]
fn every_length_is_filled_to_the_end() {
    random::fill(&mut []);
    // lengths around the 256-byte limit of one macOS request, and odd ones
    for length in [1usize, 7, 31, 255, 256, 257, 513, 4099, 70_001] {
        let mut bytes = vec![0u8; length];
        random::try_fill(&mut bytes).unwrap();
        if length >= 31 {
            // sixteen zero bytes in a row do not come from a working generator
            let tail = &bytes[length - 16..];
            assert!(tail.iter().any(|byte| *byte != 0), "length {length}");
            let head = &bytes[..16];
            assert!(head.iter().any(|byte| *byte != 0), "length {length}");
        }
    }
}

#[test]
fn bytes_take_many_values() {
    let mut bytes = [0u8; 4096];
    random::fill(&mut bytes);
    let mut seen = [false; 256];
    for byte in bytes {
        seen[usize::from(byte)] = true;
    }
    // 4096 uniform bytes miss a given value with probability e^-16
    assert!(seen.iter().filter(|seen| **seen).count() > 250);
}
