use alt_hash::{Crc32c, crc32c};

use crate::noise;

/// bit by bit, straight from the definition
fn by_definition(data: &[u8]) -> u32 {
    let mut register = !0u32;
    for byte in data {
        register ^= *byte as u32;
        for _ in 0..8 {
            register = (register >> 1) ^ if register & 1 != 0 { 0x82f6_3b78 } else { 0 };
        }
    }
    !register
}

#[test]
fn published_vectors() {
    assert_eq!(crc32c(b""), 0);
    assert_eq!(crc32c(b"123456789"), 0xe306_9283);
    // the test patterns of RFC 3720, appendix B.4
    assert_eq!(crc32c(&[0u8; 32]), 0x8a91_36aa);
    assert_eq!(crc32c(&[0xffu8; 32]), 0x62a8_ab43);
    let rising: Vec<u8> = (0..32).collect();
    assert_eq!(crc32c(&rising), 0x46dd_794e);
    let falling: Vec<u8> = (0..32).rev().collect();
    assert_eq!(crc32c(&falling), 0x113f_db5c);
}

#[test]
fn the_instructions_and_the_tables_match_the_definition_at_every_length_and_split() {
    println!("crc32c: {}", alt_hash::crc32c::implementation());
    let data = noise(1200, 0x9e37_79b9_7f4a_7c15);
    for len in 0..data.len() {
        let expected = by_definition(&data[..len]);
        assert_eq!(crc32c(&data[..len]), expected, "length {len}");
        assert_eq!(
            !alt_hash::crc32c::portable(!0, &data[..len]),
            expected,
            "portable, length {len}"
        );
        let mut streaming = Crc32c::new();
        let cut = len / 3;
        streaming.update(&data[..cut]);
        streaming.update(&data[cut..len]);
        assert_eq!(streaming.finalize(), expected, "split at {cut} of {len}");
    }
    let large = noise(1 << 18, 11);
    for len in [4095, 4096, 4097, 65_536, 150_001, 1 << 18] {
        let expected = by_definition(&large[..len]);
        assert_eq!(crc32c(&large[..len]), expected, "length {len}");
        assert_eq!(
            !alt_hash::crc32c::portable(!0, &large[..len]),
            expected,
            "portable, length {len}"
        );
    }
}

#[test]
fn a_misaligned_start_reads_the_same() {
    let data = noise(300, 5);
    for start in 0..16 {
        assert_eq!(crc32c(&data[start..]), by_definition(&data[start..]));
    }
}
