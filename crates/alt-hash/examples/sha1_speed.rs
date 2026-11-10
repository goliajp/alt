//! throughput of the single-stream and the many-at-once SHA-1, and with the argument
//! `sha256` of SHA-256 (single stream on an in-cache 16 KiB buffer, like
//! `openssl speed -bytes 16384 sha1`)

use alt_hash::{sha1, sha1_many};
use std::time::Instant;

fn main() {
    let mode = std::env::args().nth(1);
    let buffer = vec![0x5au8; 16384];
    let mut rounds = 0u64;
    let mut sink = 0u8;
    let start = Instant::now();
    if mode.as_deref() == Some("sha256") {
        while start.elapsed().as_secs_f64() < 1.0 {
            for _ in 0..256 {
                sink ^= alt_hash::sha256(std::hint::black_box(&buffer))[0];
            }
            rounds += 256;
        }
        println!(
            "sha-256 single stream ({}), 16 KiB in cache: {:.0} MiB/s ({sink:02x})",
            alt_hash::sha256::implementation(),
            rounds as f64 * 16384.0 / (1 << 20) as f64 / start.elapsed().as_secs_f64()
        );
        return;
    }
    while mode.as_deref() != Some("many")
        && !matches!(mode.as_deref(), Some("hot"))
        && start.elapsed().as_secs_f64() < 1.0
    {
        for _ in 0..256 {
            sink ^= sha1(std::hint::black_box(&buffer))[0];
        }
        rounds += 256;
    }
    let seconds = start.elapsed().as_secs_f64();
    if mode.as_deref() != Some("many") && mode.as_deref() != Some("hot") {
        println!(
            "single stream ({}), 16 KiB in cache: {:.0} MiB/s ({sink:02x})",
            alt_hash::sha1::implementation(),
            rounds as f64 * 16384.0 / (1 << 20) as f64 / seconds
        );
    }

    if mode.as_deref() == Some("single") {
        return;
    }
    // 64 messages stay in the second-level cache; 8192 do not
    // `hot` keeps to the small batch, long enough to sample
    let hot = mode.as_deref() == Some("hot");
    for count in if hot {
        vec![64usize; 6]
    } else {
        vec![64, 8192]
    } {
        let small: Vec<Vec<u8>> = (0..count)
            .map(|index| vec![index as u8; 700 + index % 600])
            .collect();
        let total: usize = small.iter().map(Vec::len).sum();
        let parts: Vec<[&[u8]; 1]> = small.iter().map(|body| [body.as_slice()]).collect();
        let messages: Vec<&[&[u8]]> = parts.iter().map(|part| part.as_slice()).collect();
        let reference: Vec<[u8; 20]> = small.iter().map(|body| sha1(body)).collect();
        let mut best = (f64::MAX, f64::MAX);
        for _ in 0..(64 << 20) / total {
            let start = Instant::now();
            let one: Vec<[u8; 20]> = small.iter().map(|body| sha1(body)).collect();
            let single = start.elapsed().as_secs_f64();
            let start = Instant::now();
            let many = sha1_many(&messages);
            let together = start.elapsed().as_secs_f64();
            assert_eq!(one, reference);
            assert_eq!(many, reference);
            best = (best.0.min(single), best.1.min(together));
        }
        let mib = total as f64 / (1 << 20) as f64;
        println!(
            "{count} messages of about 1 KiB: one at a time {:.0} MiB/s, together {:.0} MiB/s",
            mib / best.0,
            mib / best.1
        );
    }
}
