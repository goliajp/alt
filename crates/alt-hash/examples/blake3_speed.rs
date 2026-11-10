//! throughput of BLAKE3: one-shot `hash` at several sizes, and the streaming hasher
//! fed 64 KiB at a time

use alt_hash::blake3;
use std::hint::black_box;
use std::time::Instant;

/// MiB/s of the best of several runs, each hashing about `budget` bytes
fn best(len: usize, budget: usize, mut run: impl FnMut() -> u8) -> f64 {
    let rounds = (budget / len.max(1)).max(1);
    let mut best = f64::MAX;
    let mut sink = 0;
    for _ in 0..if len > 1 << 20 { 7 } else { 25 } {
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
    println!("blake3 ({})", blake3::implementation());
    let data: Vec<u8> = (0..64usize << 20)
        .map(|index| ((index * 31) >> 3) as u8)
        .collect();
    let mut sizes = vec![
        ("64 bytes".to_string(), 64usize),
        ("1 KiB".to_string(), 1 << 10),
        ("16 KiB".to_string(), 16 << 10),
        ("1 MiB".to_string(), 1 << 20),
        ("64 MiB".to_string(), 64 << 20),
    ];
    // further sizes in bytes may be given as arguments
    for len in std::env::args().skip(1) {
        sizes.push((len.clone(), len.parse().unwrap()));
    }
    for (name, len) in sizes {
        let input = &data[..len];
        let budget = if len > 1 << 20 { len } else { 8 << 20 };
        let speed = best(len, budget, || blake3::hash(black_box(input)).as_bytes()[0]);
        println!("one-shot {name:>8}: {speed:>6.0} MiB/s");
    }
    let speed = best(data.len(), data.len(), || {
        let mut hasher = blake3::Hasher::new();
        for piece in black_box(&data).chunks(64 << 10) {
            hasher.update(piece);
        }
        hasher.finalize().as_bytes()[0]
    });
    println!("streaming, 64 KiB pieces: {speed:>6.0} MiB/s");
}
