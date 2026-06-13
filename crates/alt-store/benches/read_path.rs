//! Read-path decomposition: where the `.alt` read time goes. S10 measured
//! direct reads at ~3-5x slower than git; S1 showed the components (BLAKE3
//! 2-18 GiB/s, zstd lineage decode ~831 MiB/s). This bench reads chunks at
//! increasing delta-chain depth: a plain chunk is one decode + one re-hash,
//! a depth-D chain is D decodes + D re-hashes. The slope across depth
//! attributes the read cost to per-layer unchain vs the fixed read floor.

use alt_store::{ChunkId, ChunkStore};
use criterion::{Criterion, Throughput, black_box, criterion_group, criterion_main};

const CHUNK: usize = 64 << 10;

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

/// One byte in `step` flipped — a tiny edit so the lineage delta is small and
/// `reencode_as_delta` always accepts it, keeping the chain intact.
fn tweak(prev: &[u8], step: usize) -> Vec<u8> {
    let mut v = prev.to_vec();
    for i in (step % 7..v.len()).step_by(prev.len() / 64) {
        v[i] ^= 0x5a;
    }
    v
}

/// Builds a delta chain of the given depth in `store` and returns the id of
/// its tail (the deepest, most expensive chunk to read).
fn build_chain(store: &mut ChunkStore, depth: usize, seed: u64) -> ChunkId {
    let mut data = fill(CHUNK, seed);
    let mut id = store.put(&data).unwrap();
    let mut prev = id;
    for d in 0..depth {
        data = tweak(&data, d + 1);
        id = store.put(&data).unwrap();
        assert!(
            store.reencode_as_delta(id, prev).unwrap(),
            "depth {d}: lineage delta must form for the bench",
        );
        prev = id;
    }
    id
}

fn bench(c: &mut Criterion) {
    let dir = tempfile::tempdir().unwrap();
    let mut store = ChunkStore::open(dir.path()).unwrap();

    // a plain chunk (no delta) and chain tails at depth 4 / 8 / 12 (the
    // import depth cap refuses deltas past ~13)
    let plain = store.put(&fill(CHUNK, 0xA1)).unwrap();
    let d4 = build_chain(&mut store, 4, 0xB2);
    let d8 = build_chain(&mut store, 8, 0xC3);
    let d12 = build_chain(&mut store, 12, 0xD4);
    store.flush().unwrap();

    let mut g = c.benchmark_group("read");
    g.throughput(Throughput::Bytes(CHUNK as u64));
    for (name, id) in [
        ("plain", plain),
        ("delta_d4", d4),
        ("delta_d8", d8),
        ("delta_d12", d12),
    ] {
        g.bench_function(name, |b| {
            b.iter(|| black_box(store.get(black_box(id)).unwrap()))
        });
    }
    g.finish();

    // attribution: split a plain 64 KiB read into its two parts — the zstd
    // decode and the BLAKE3 re-hash — to see which dominates `get()`
    let raw = fill(CHUNK, 0xA1);
    let comp = zstd::encode_all(&raw[..], 3).unwrap();
    let mut g = c.benchmark_group("read_parts");
    g.throughput(Throughput::Bytes(CHUNK as u64));
    g.bench_function("zstd_decode_64kib", |b| {
        b.iter(|| black_box(zstd::decode_all(black_box(&comp[..])).unwrap()))
    });
    g.bench_function("blake3_rehash_64kib", |b| {
        b.iter(|| black_box(ChunkId::of(black_box(&raw))))
    });
    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
