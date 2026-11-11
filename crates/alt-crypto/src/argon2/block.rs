//! The compression function G of RFC 9106 section 3.5 on 1024-byte blocks.

#[cfg(target_arch = "x86_64")]
mod avx2;
#[cfg(target_arch = "aarch64")]
mod neon;

pub type Block = [u64; 128];

/// `out = G(previous, reference)`, xored into what `out` held when `xor` is set
pub(super) type Compress = fn(&Block, &Block, &mut Block, bool);

type Accelerated = unsafe fn(&Block, &Block, &mut Block, bool);

/// the fastest implementation this CPU supports, and its name
fn select() -> (&'static str, Option<Accelerated>) {
    #[cfg(target_arch = "aarch64")]
    if std::arch::is_aarch64_feature_detected!("neon") {
        return ("neon", Some(neon::compress));
    }
    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("avx2") {
        return ("avx2", Some(avx2::compress));
    }
    ("portable", None)
}

/// which implementation runs on this CPU
pub fn implementation() -> &'static str {
    select().0
}

/// `out = G(previous, reference)`, xored into what `out` held when `xor` is set.
pub fn compress(previous: &Block, reference: &Block, out: &mut Block, xor: bool) {
    match select().1 {
        // an implementation is only selected after its CPU features were detected
        Some(accelerated) => unsafe { accelerated(previous, reference, out, xor) },
        None => portable(previous, reference, out, xor),
    }
}

/// The reference implementation every accelerated one is compared with.
pub fn portable(previous: &Block, reference: &Block, out: &mut Block, xor: bool) {
    let mut mixed = *previous;
    for (word, other) in mixed.iter_mut().zip(reference) {
        *word ^= other;
    }
    let mut permuted = mixed;
    permute(&mut permuted);
    for i in 0..128 {
        let word = permuted[i] ^ mixed[i];
        out[i] = if xor { out[i] ^ word } else { word };
    }
}

macro_rules! mix {
    ($a:ident, $b:ident, $c:ident, $d:ident) => {
        $a = multiply_add($a, $b);
        $d = ($d ^ $a).rotate_right(32);
        $c = multiply_add($c, $d);
        $b = ($b ^ $c).rotate_right(24);
        $a = multiply_add($a, $b);
        $d = ($d ^ $a).rotate_right(16);
        $c = multiply_add($c, $d);
        $b = ($b ^ $c).rotate_right(63);
    };
}

/// the BLAKE2b round with multiplications, over sixteen words of the block
macro_rules! round {
    ($v:ident, $($i:expr),+) => {{
        let [mut v0, mut v1, mut v2, mut v3, mut v4, mut v5, mut v6, mut v7,
             mut v8, mut v9, mut v10, mut v11, mut v12, mut v13, mut v14, mut v15] = [$($v[$i]),+];
        mix!(v0, v4, v8, v12);
        mix!(v1, v5, v9, v13);
        mix!(v2, v6, v10, v14);
        mix!(v3, v7, v11, v15);
        mix!(v0, v5, v10, v15);
        mix!(v1, v6, v11, v12);
        mix!(v2, v7, v8, v13);
        mix!(v3, v4, v9, v14);
        let after = [v0, v1, v2, v3, v4, v5, v6, v7, v8, v9, v10, v11, v12, v13, v14, v15];
        for (at, word) in [$($i),+].into_iter().zip(after) {
            $v[at] = word;
        }
    }};
}

/// a + b + 2 * low32(a) * low32(b)
#[inline(always)]
fn multiply_add(a: u64, b: u64) -> u64 {
    let product = (a & 0xffff_ffff) * (b & 0xffff_ffff);
    a.wrapping_add(b).wrapping_add(product << 1)
}

/// the permutation P applied to the eight rows, then the eight columns
fn permute(v: &mut Block) {
    for row in 0..8 {
        let r = 16 * row;
        round!(
            v,
            r,
            r + 1,
            r + 2,
            r + 3,
            r + 4,
            r + 5,
            r + 6,
            r + 7,
            r + 8,
            r + 9,
            r + 10,
            r + 11,
            r + 12,
            r + 13,
            r + 14,
            r + 15
        );
    }
    for column in 0..8 {
        let c = 2 * column;
        round!(
            v,
            c,
            c + 1,
            c + 16,
            c + 17,
            c + 32,
            c + 33,
            c + 48,
            c + 49,
            c + 64,
            c + 65,
            c + 80,
            c + 81,
            c + 96,
            c + 97,
            c + 112,
            c + 113
        );
    }
}
