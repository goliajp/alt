//! A fast keyed hash for interning lines. Diffing hashes every line of
//! both files, and the default hasher is most of that cost; this one
//! folds eight bytes at a time through a 64x64 multiplication, seeded per
//! process so that colliding lines cannot be prepared in advance.

use std::hash::{BuildHasher, Hasher};
use std::sync::OnceLock;

const MIX: u64 = 0x9e37_79b9_7f4a_7c15;
const LAST: u64 = 0xd6e8_feb8_6659_fd93;

fn fold(a: u64, b: u64) -> u64 {
    let wide = u128::from(a) * u128::from(b);
    (wide as u64) ^ ((wide >> 64) as u64)
}

pub struct FoldHasher(u64);

impl Hasher for FoldHasher {
    fn write(&mut self, bytes: &[u8]) {
        let (words, rest) = bytes.as_chunks::<8>();
        for word in words {
            self.0 = fold(self.0 ^ u64::from_le_bytes(*word), MIX);
        }
        if !rest.is_empty() {
            let mut last = [0u8; 8];
            last[..rest.len()].copy_from_slice(rest);
            self.0 = fold(self.0 ^ u64::from_le_bytes(last), MIX);
        }
    }

    fn write_usize(&mut self, n: usize) {
        self.0 = fold(self.0 ^ n as u64, MIX);
    }

    fn finish(&self) -> u64 {
        fold(self.0, LAST)
    }
}

/// Builds [`FoldHasher`]s that share the seed of this process.
#[derive(Clone, Copy, Default)]
pub struct FoldState;

impl BuildHasher for FoldState {
    type Hasher = FoldHasher;

    fn build_hasher(&self) -> FoldHasher {
        static SEED: OnceLock<u64> = OnceLock::new();
        let seed = SEED.get_or_init(|| std::hash::RandomState::new().hash_one(0u8));
        FoldHasher(*seed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_that_differ_anywhere_hash_apart() {
        let hash = |line: &[u8]| FoldState.hash_one(line);
        let lines: [&[u8]; 7] = [
            b"",
            b"a",
            b"b",
            b"12345678",
            b"123456789",
            b"12345678\0",
            b"a line of ordinary length\n",
        ];
        for (i, a) in lines.iter().enumerate() {
            assert_eq!(hash(a), hash(a));
            for b in &lines[i + 1..] {
                assert_ne!(hash(a), hash(b), "{a:?} {b:?}");
            }
        }
    }
}
