//! A `Hasher` for keys that are digests already: content addresses, object
//! ids. Their bytes are uniformly distributed, so the table index is read
//! straight out of them instead of being computed by a keyed hash — and a
//! map built on it needs no random seed from the operating system.

use std::hash::{BuildHasherDefault, Hasher};

/// Folds the first eight bytes of each write into the state.
#[derive(Default, Clone, Copy)]
pub struct IdHasher(u64);

impl Hasher for IdHasher {
    fn write(&mut self, bytes: &[u8]) {
        let mut word = [0u8; 8];
        let taken = bytes.len().min(8);
        word[..taken].copy_from_slice(&bytes[..taken]);
        self.0 = self.0.rotate_left(5) ^ u64::from_le_bytes(word);
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

pub type IdBuildHasher = BuildHasherDefault<IdHasher>;
/// A hash map keyed by digests.
pub type IdMap<K, V> = std::collections::HashMap<K, V, IdBuildHasher>;
/// A hash set of digests.
pub type IdSet<K> = std::collections::HashSet<K, IdBuildHasher>;
