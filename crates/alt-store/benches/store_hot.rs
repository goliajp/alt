//! Store hot-path ceilings. Two paths dominate:
//!
//!   - BLAKE3 hashing — the read re-verification cost. Every read re-hashes
//!     the bytes it returns; the default fast path will do this once at the
//!     object boundary (M3.5 阶段 B), so this number is the read floor.
//!   - lineage delta codec (zstd ref-prefix) — encode runs per changed blob
//!     at import, decode runs per delta layer at read.

use alt_store::{BlobId, ChunkId, delta};
use criterion::{Criterion, Throughput, black_box, criterion_group, criterion_main};

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

/// `base` with ~5% of its bytes flipped in scattered runs — the shape a
/// lineage delta is meant to capture (a small edit to a prior version).
fn perturb(base: &[u8], mut s: u64) -> Vec<u8> {
    let mut v = base.to_vec();
    let edits = base.len() / 20;
    for _ in 0..edits {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        let i = (s as usize) % v.len();
        v[i] ^= 0xff;
    }
    v
}

fn bench(c: &mut Criterion) {
    // --- BLAKE3 hashing: the read re-verify floor ---
    let big = fill(8 << 20, 0x1111_2222_3333_4444);
    let small = fill(64 << 10, 0x5555_6666_7777_8888);
    let mut g = c.benchmark_group("hash");
    g.throughput(Throughput::Bytes(big.len() as u64));
    g.bench_function("blob_blake3_8mib", |b| {
        b.iter(|| black_box(BlobId::of(black_box(&big))))
    });
    g.throughput(Throughput::Bytes(small.len() as u64));
    g.bench_function("chunk_blake3_64kib", |b| {
        b.iter(|| black_box(ChunkId::of(black_box(&small))))
    });
    g.finish();

    // --- lineage delta codec (zstd level 3, the store default) ---
    let base = fill(1 << 20, 0x9999_AAAA_BBBB_CCCC);
    let data = perturb(&base, 0xDDDD_EEEE_FFFF_0000);
    let payload = delta::compress_with_base(&data, &base, 3).expect("compress");
    let mut g = c.benchmark_group("delta");
    g.throughput(Throughput::Bytes(data.len() as u64));
    g.bench_function("encode_1mib_l3", |b| {
        b.iter(|| {
            black_box(delta::compress_with_base(
                black_box(&data),
                black_box(&base),
                3,
            ))
        })
    });
    g.bench_function("decode_1mib", |b| {
        b.iter(|| {
            black_box(delta::decompress_with_base(
                black_box(&payload),
                black_box(&base),
                data.len(),
            ))
        })
    });
    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
