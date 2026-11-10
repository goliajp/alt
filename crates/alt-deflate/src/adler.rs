const BASE: u32 = 65521;
/// largest number of bytes whose sums cannot overflow u32 before reduction
const CHUNK: usize = 5552;

/// Adler-32 checksum of `data`.
pub fn adler32(data: &[u8]) -> u32 {
    let mut a = 1u32;
    let mut b = 0u32;
    for chunk in data.chunks(CHUNK) {
        for &byte in chunk {
            a += u32::from(byte);
            b += a;
        }
        a %= BASE;
        b %= BASE;
    }
    (b << 16) | a
}
