//! throughput of the single-stream and the many-at-once SHA-1 on small and large messages

use alt_hash::{sha1, sha1_many};
use std::time::Instant;

fn main() {
    let big = vec![0x5au8; 64 << 20];
    let start = Instant::now();
    let digest = sha1(&big);
    let seconds = start.elapsed().as_secs_f64();
    println!(
        "single stream, 64 MiB: {:.0} MiB/s ({:02x}{:02x})",
        64.0 / seconds,
        digest[0],
        digest[1]
    );

    let small: Vec<Vec<u8>> = (0..8192usize)
        .map(|index| vec![index as u8; 700 + index % 600])
        .collect();
    let total: usize = small.iter().map(Vec::len).sum();
    let parts: Vec<[&[u8]; 1]> = small.iter().map(|body| [body.as_slice()]).collect();
    let messages: Vec<&[&[u8]]> = parts.iter().map(|part| part.as_slice()).collect();
    for round in 0..3 {
        let start = Instant::now();
        let one: Vec<[u8; 20]> = small.iter().map(|body| sha1(body)).collect();
        let single = start.elapsed().as_secs_f64();
        let start = Instant::now();
        let many = sha1_many(&messages);
        let together = start.elapsed().as_secs_f64();
        assert_eq!(one, many);
        if round == 2 {
            let mib = total as f64 / (1 << 20) as f64;
            println!(
                "8192 messages of about 1 KiB: one at a time {:.0} MiB/s, together {:.0} MiB/s",
                mib / single,
                mib / together
            );
        }
    }
}
