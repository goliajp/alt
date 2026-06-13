//! Git delta application ceiling: resolving an ofs/ref-delta entry against
//! its base is the per-object cost of reading a packed (delta) object, which
//! every corpus pack read goes through.

use alt_git_pack::delta;
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

/// git size varint: 7 bits per byte, low first, high bit continues.
fn put_size(mut n: u64, out: &mut Vec<u8>) {
    loop {
        let mut b = (n & 0x7f) as u8;
        n >>= 7;
        if n != 0 {
            b |= 0x80;
        }
        out.push(b);
        if n == 0 {
            break;
        }
    }
}

/// copy `len` bytes from base `off`: 0x80 | present-byte flags, then the
/// nonzero little-endian offset/size bytes.
fn put_copy(off: u32, len: u32, out: &mut Vec<u8>) {
    let mut op = 0x80u8;
    let mut ops = Vec::new();
    for i in 0..4 {
        let byte = (off >> (8 * i)) as u8;
        if byte != 0 {
            op |= 1 << i;
            ops.push(byte);
        }
    }
    for i in 0..3 {
        let byte = (len >> (8 * i)) as u8;
        if byte != 0 {
            op |= 0x10 << i;
            ops.push(byte);
        }
    }
    out.push(op);
    out.extend_from_slice(&ops);
}

fn put_insert(bytes: &[u8], out: &mut Vec<u8>) {
    for ch in bytes.chunks(127) {
        out.push(ch.len() as u8);
        out.extend_from_slice(ch);
    }
}

fn bench(c: &mut Criterion) {
    let base = fill(1 << 20, 0x0F0F_1E1E_2D2D_3C3C);
    let half = base.len() / 2;
    let lit = fill(4096, 0xC3C3_B4B4_A5A5_9696);

    // copy first half, insert a literal run, copy second half — a realistic
    // mostly-copy delta with one inserted region
    let mut d = Vec::new();
    put_size(base.len() as u64, &mut d);
    let tgt = half + lit.len() + (base.len() - half);
    put_size(tgt as u64, &mut d);
    put_copy(0, half as u32, &mut d);
    put_insert(&lit, &mut d);
    put_copy(half as u32, (base.len() - half) as u32, &mut d);

    let applied = delta::apply(&base, &d).expect("the hand-built delta must apply");
    assert_eq!(applied.len(), tgt);

    let mut g = c.benchmark_group("pack");
    g.throughput(Throughput::Bytes(tgt as u64));
    g.bench_function("delta_apply_1mib", |b| {
        b.iter(|| black_box(delta::apply(black_box(&base), black_box(&d))))
    });
    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
