//! one checksum pass per argument, for counting instructions per byte:
//! `crc32c_cost <instructions|portable> <length> <passes>`

use std::hint::black_box;

fn main() {
    let mut args = std::env::args().skip(1);
    let which = args.next().unwrap();
    let len: usize = args.next().unwrap().parse().unwrap();
    let passes: usize = args.next().unwrap().parse().unwrap();
    let data: Vec<u8> = (0..len).map(|index| ((index * 31) >> 3) as u8).collect();
    let mut sink = 0u32;
    for _ in 0..passes {
        sink ^= match which.as_str() {
            "portable" => alt_hash::crc32c::portable(!0, black_box(&data)),
            _ => alt_hash::crc32c(black_box(&data)),
        };
    }
    println!(
        "{} {len} {passes} {sink:08x}",
        alt_hash::crc32c::implementation()
    );
}
