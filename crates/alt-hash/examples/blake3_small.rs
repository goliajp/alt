//! throughput of one-shot BLAKE3 on inputs of a few chunks or less, by every
//! implementation this CPU supports

use alt_hash::blake3::{self, Backend};
use std::hint::black_box;
use std::time::Instant;

/// MiB/s of the best of several runs over `inputs`
fn best(inputs: &[&[u8]], mut run: impl FnMut(&[u8]) -> u8) -> f64 {
    let bytes: usize = inputs.iter().map(|input| input.len()).sum();
    let mut best = f64::MAX;
    let mut sink = 0;
    for _ in 0..15 {
        let start = Instant::now();
        for input in inputs {
            sink ^= run(black_box(input));
        }
        best = best.min(start.elapsed().as_secs_f64());
    }
    black_box(sink);
    bytes as f64 / (1 << 20) as f64 / best
}

fn main() {
    let data: Vec<u8> = (0..4usize << 20)
        .map(|index| ((index * 31) >> 3) as u8)
        .collect();
    let mut sizes = vec![
        64, 256, 512, 1024, 1025, 1536, 2048, 2049, 3072, 3600, 4096, 8192,
    ];
    // other sizes in bytes may be given as arguments instead
    let given: Vec<usize> = std::env::args()
        .skip(1)
        .map(|len| len.parse().unwrap())
        .collect();
    if !given.is_empty() {
        sizes = given;
    }
    let backends = Backend::supported();
    print!("{:>6}", "bytes");
    for backend in &backends {
        print!("  {:>30}", backend.name());
    }
    println!(
        "  {:>10}  MiB/s; the default is {}",
        "hash_many",
        blake3::implementation()
    );
    for len in sizes {
        // inputs that lie apart, as separate objects do
        let inputs: Vec<&[u8]> = data.chunks_exact(len).take((2 << 20) / len).collect();
        print!("{len:>6}");
        for backend in &backends {
            let speed = best(&inputs, |input| {
                blake3::hash_with(*backend, input).as_bytes()[0]
            });
            print!("  {speed:>30.0}");
        }
        // the same inputs handed over 32 at a time, as ingest does
        let bytes = (inputs.len() * len) as f64 / (1 << 20) as f64;
        let mut fastest = f64::MAX;
        for _ in 0..15 {
            let start = Instant::now();
            for batch in inputs.chunks(32) {
                black_box(blake3::hash_many(black_box(batch)));
            }
            fastest = fastest.min(start.elapsed().as_secs_f64());
        }
        println!("  {:>10.0}", bytes / fastest);
    }
}
