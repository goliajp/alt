//! throughput of SHA-512: the implementation this CPU selects against the portable one

use alt_hash::sha512::{self, INITIAL};
use std::hint::black_box;
use std::time::Instant;

/// MiB/s of the best of several runs, each hashing about 8 MiB
fn best(len: usize, mut run: impl FnMut() -> u8) -> f64 {
    let rounds = ((8 << 20) / len).max(1);
    let mut best = f64::MAX;
    let mut sink = 0;
    for _ in 0..15 {
        let start = Instant::now();
        for _ in 0..rounds {
            sink ^= run();
        }
        best = best.min(start.elapsed().as_secs_f64());
    }
    black_box(sink);
    (rounds * len) as f64 / (1 << 20) as f64 / best
}

fn main() {
    println!("sha512 ({})", sha512::implementation());
    let data: Vec<u8> = (0..1usize << 20)
        .map(|index| ((index * 31) >> 3) as u8)
        .collect();
    for len in [128usize, 1 << 10, 16 << 10, 1 << 20] {
        let input = &data[..len];
        let selected = best(len, || alt_hash::sha512(black_box(input))[0]);
        // whole blocks only, which is all the compression function takes
        let portable = best(len, || {
            let mut state = INITIAL;
            sha512::portable(&mut state, black_box(input));
            state[0] as u8
        });
        println!(
            "{len:>8} bytes: selected {selected:>6.0} MiB/s, portable compression {portable:>6.0} MiB/s"
        );
    }
}
