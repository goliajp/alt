//! A `Hasher` for keys that are digests already: content addresses, object
//! ids. Their bytes are uniformly distributed, so the table index is read
//! straight out of them instead of being computed by a keyed hash — and a
//! map built on it needs no random seed from the operating system.

use std::hash::{BuildHasher, BuildHasherDefault, Hasher, RandomState};
use std::sync::OnceLock;

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

/// [`IdHasher`] for digests a peer chooses: the ids of the objects it
/// sends. Whoever can pick the keys of a table can pick them to share
/// the bits a bare id is placed by, at the price of a few hashes each,
/// and make every lookup walk through all of them. Here the id is
/// multiplied with a number of this process that the peer does not
/// know, and placed by both halves of the product.
#[derive(Clone, Copy)]
pub struct KeyedIdHasher {
    state: u64,
    key: [u64; 2],
}

impl KeyedIdHasher {
    /// Takes `word` in. A key of several digests is folded with the
    /// number of the process too: folded bare, a peer could choose
    /// pairs of digests that fold alike.
    fn mix(&mut self, word: u64) {
        self.state = (self.state.rotate_left(5) ^ word).wrapping_mul(self.key[1]);
    }
}

impl Hasher for KeyedIdHasher {
    fn write(&mut self, bytes: &[u8]) {
        let word = match bytes.first_chunk::<8>() {
            Some(word) => *word,
            None => {
                let mut word = [0u8; 8];
                word[..bytes.len()].copy_from_slice(bytes);
                word
            }
        };
        self.mix(u64::from_le_bytes(word));
    }

    // the length of a slice and the variant of an enum, ahead of the digest
    fn write_usize(&mut self, n: usize) {
        self.mix(n as u64);
    }

    fn finish(&self) -> u64 {
        let product = u128::from(self.state ^ self.key[0]) * u128::from(self.key[1]);
        product as u64 ^ (product >> 64) as u64
    }
}

/// Builds [`KeyedIdHasher`]s with the key of this process.
#[derive(Clone, Copy)]
pub struct KeyedIdBuildHasher([u64; 2]);

impl Default for KeyedIdBuildHasher {
    fn default() -> Self {
        static KEY: OnceLock<[u64; 2]> = OnceLock::new();
        // the one source of randomness the standard library has
        let drawn = || RandomState::new().build_hasher().finish();
        KeyedIdBuildHasher(*KEY.get_or_init(|| [drawn(), drawn() | 1]))
    }
}

impl BuildHasher for KeyedIdBuildHasher {
    type Hasher = KeyedIdHasher;

    fn build_hasher(&self) -> KeyedIdHasher {
        let (state, key) = (0, self.0);
        KeyedIdHasher { state, key }
    }
}

/// A hash map keyed by digests a peer chooses.
pub type KeyedIdMap<K, V> = std::collections::HashMap<K, V, KeyedIdBuildHasher>;
/// A hash set of digests a peer chooses.
pub type KeyedIdSet<K> = std::collections::HashSet<K, KeyedIdBuildHasher>;

#[cfg(test)]
mod tests {
    use std::hash::BuildHasher;

    use super::{IdBuildHasher, KeyedIdBuildHasher};

    /// Ids that differ in the bits a table of a million slots is not
    /// indexed by: bare, they all fall into one slot; keyed, into about
    /// as many slots as there are ids.
    #[test]
    fn ids_chosen_to_share_their_low_bits_are_spread_by_the_key() {
        let ids: Vec<[u8; 20]> = (1u64..=4096)
            .map(|n| {
                let mut id = [0x5a; 20];
                id[..8].copy_from_slice(&(n << 20 | 0xabcde).to_le_bytes());
                id
            })
            .collect();
        let slots = |hashes: Vec<u64>| {
            let mut slots: Vec<u64> = hashes.iter().map(|hash| hash & 0xf_ffff).collect();
            slots.sort_unstable();
            slots.dedup();
            slots.len()
        };
        let bare = IdBuildHasher::default();
        assert_eq!(slots(ids.iter().map(|id| bare.hash_one(id)).collect()), 1);
        let keyed = KeyedIdBuildHasher::default();
        let spread = slots(ids.iter().map(|id| keyed.hash_one(id)).collect());
        assert!(spread > 4000, "{spread} slots for 4096 ids");
        // and the control byte of the table, the top seven bits
        let tops = |hashes: Vec<u64>| slots(hashes.iter().map(|hash| hash >> 57).collect());
        assert_eq!(tops(ids.iter().map(|id| keyed.hash_one(id)).collect()), 128);
    }

    /// Pairs of ids chosen so that the two fold to the same word where
    /// they are folded bare: keyed, each pair has a hash of its own.
    #[test]
    fn pairs_of_ids_chosen_to_fold_alike_are_told_apart_by_the_key() {
        // a pair is taken in as the length of each id and its first word
        let words = |change: u64| {
            let first = 0x1234_5678_9abc_def0u64 ^ change;
            // what the fold turns the first by before the second is taken in
            let second = 0x0fed_cba9_8765_4321u64 ^ change.rotate_left(10);
            (first, second)
        };
        let pair = |change: u64| {
            let (mut first, mut second) = ([0x5au8; 20], [0xa5u8; 20]);
            first[..8].copy_from_slice(&words(change).0.to_le_bytes());
            second[..8].copy_from_slice(&words(change).1.to_le_bytes());
            (first, second)
        };
        let bare = |change: u64| {
            let taken = [20, words(change).0, 20, words(change).1];
            (taken.iter()).fold(0u64, |state, word| state.rotate_left(5) ^ word)
        };
        assert!((1u64..=4096).all(|n| bare(n << 13) == bare(0)));
        let keyed = KeyedIdBuildHasher::default();
        let mut hashes: Vec<u64> = (1u64..=4096)
            .map(|n| keyed.hash_one(pair(n << 13)))
            .collect();
        hashes.sort_unstable();
        hashes.dedup();
        assert_eq!(hashes.len(), 4096);
    }

    #[test]
    fn a_key_hashes_alike_in_every_table_of_the_process() {
        let (one, other) = (KeyedIdBuildHasher::default(), KeyedIdBuildHasher::default());
        let id = [7u8; 32];
        assert_eq!(one.hash_one(id), other.hash_one(id));
        assert_ne!(one.hash_one(id), one.hash_one([8u8; 32]));
    }
}
