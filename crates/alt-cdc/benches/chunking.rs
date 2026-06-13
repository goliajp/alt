//! Chunking throughput ceiling: how fast FastCDC carves a buffer. The
//! number bounds import's content-defined-chunking stage.

use criterion::{Criterion, Throughput, black_box, criterion_group, criterion_main};

/// Deterministic pseudo-random bytes (xorshift64): entropy enough that the
/// gear hash hits cut masks at realistic spacing, reproducible run to run.
fn fill(len: usize, mut s: u64) -> Vec<u8> {
    let mut v = Vec::with_capacity(len + 8);
    while v.len() < len {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        v.extend_from_slice(&s.to_le_bytes());
    }
    v.truncate(len);
    v
}

fn bench(c: &mut Criterion) {
    let data = fill(8 << 20, 0x9E37_79B9_7F4A_7C15);
    let mut g = c.benchmark_group("cdc");
    g.throughput(Throughput::Bytes(data.len() as u64));
    g.bench_function("chunks_8mib", |b| {
        b.iter(|| {
            let mut total = 0usize;
            for ch in alt_cdc::chunks(black_box(&data), alt_cdc::DEFAULT_PARAMS) {
                total += ch.len();
            }
            black_box(total)
        })
    });
    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
