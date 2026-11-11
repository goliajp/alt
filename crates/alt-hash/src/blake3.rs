//! BLAKE3, with the 256-bit output only.
//!
//! The input is a binary tree of 1024-byte chunks. Chunks and parent nodes are
//! independent of their neighbours, so the accelerated implementations compress
//! several of them at once, one per vector lane.

mod digest;
mod hasher;
mod lanes;
pub mod portable;
mod tree;

#[cfg(target_arch = "x86_64")]
mod avx2;
#[cfg(target_arch = "aarch64")]
mod neon;
#[cfg(target_arch = "x86_64")]
mod sse41;

pub use digest::{Hash, Hex, HexError};
pub use hasher::Hasher;

/// bytes in a hash
pub const OUT_LEN: usize = 32;
/// bytes in a key
pub const KEY_LEN: usize = 32;

const CHUNK_LEN: usize = 1024;
/// the most chunks or parents any implementation compresses at once
const MAX_LANES: usize = 8;

const CHUNK_START: u32 = 1 << 0;
const CHUNK_END: u32 = 1 << 1;
const PARENT: u32 = 1 << 2;
const ROOT: u32 = 1 << 3;
const KEYED_HASH: u32 = 1 << 4;
const DERIVE_KEY_CONTEXT: u32 = 1 << 5;
const DERIVE_KEY_MATERIAL: u32 = 1 << 6;

const IV: [u32; 8] = [
    0x6a09_e667,
    0xbb67_ae85,
    0x3c6e_f372,
    0xa54f_f53a,
    0x510e_527f,
    0x9b05_688c,
    0x1f83_d9ab,
    0x5be0_cd19,
];

/// where each message word moves between two rounds
const PERMUTATION: [usize; 16] = [2, 6, 3, 10, 7, 0, 4, 13, 1, 11, 12, 5, 9, 14, 15, 8];

/// the message word each round feeds to each of its sixteen additions
const SCHEDULE: [[usize; 16]; 7] = {
    let mut schedule = [[0; 16]; 7];
    let mut word = 0;
    while word < 16 {
        schedule[0][word] = word;
        word += 1;
    }
    let mut round = 1;
    while round < 7 {
        let mut word = 0;
        while word < 16 {
            schedule[round][word] = schedule[round - 1][PERMUTATION[word]];
            word += 1;
        }
        round += 1;
    }
    schedule
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Portable,
    #[cfg(target_arch = "aarch64")]
    Neon,
    #[cfg(target_arch = "aarch64")]
    NeonPaired,
    #[cfg(target_arch = "x86_64")]
    Sse41,
    #[cfg(target_arch = "x86_64")]
    Avx2,
}

/// One implementation of the compression function. A value other than the portable
/// one only exists after its CPU features were detected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Backend(Kind);

impl Backend {
    /// the reference implementation every accelerated one is compared with
    pub const fn portable() -> Self {
        Self(Kind::Portable)
    }

    /// the fastest implementation this CPU supports
    #[inline]
    pub fn detect() -> Self {
        #[cfg(target_arch = "aarch64")]
        if std::arch::is_aarch64_feature_detected!("neon") {
            // Apple's cores have four vector units, which one group of four lanes
            // leaves half idle; not measured on other cores, so they keep one group
            return Self(if cfg!(target_vendor = "apple") {
                Kind::NeonPaired
            } else {
                Kind::Neon
            });
        }
        #[cfg(target_arch = "x86_64")]
        {
            if std::arch::is_x86_feature_detected!("avx2") {
                return Self(Kind::Avx2);
            }
            if std::arch::is_x86_feature_detected!("sse4.1") {
                return Self(Kind::Sse41);
            }
        }
        Self::portable()
    }

    /// every implementation this CPU supports, the portable one first
    pub fn supported() -> Vec<Self> {
        let mut all = vec![Self::portable()];
        #[cfg(target_arch = "aarch64")]
        if std::arch::is_aarch64_feature_detected!("neon") {
            all.push(Self(Kind::Neon));
            all.push(Self(Kind::NeonPaired));
        }
        #[cfg(target_arch = "x86_64")]
        {
            if std::arch::is_x86_feature_detected!("sse4.1") {
                all.push(Self(Kind::Sse41));
            }
            if std::arch::is_x86_feature_detected!("avx2") {
                all.push(Self(Kind::Avx2));
            }
        }
        all
    }

    pub fn name(self) -> &'static str {
        match self.0 {
            Kind::Portable => "portable",
            #[cfg(target_arch = "aarch64")]
            Kind::Neon => "neon, four chunks at a time",
            #[cfg(target_arch = "aarch64")]
            Kind::NeonPaired => "neon, eight chunks at a time",
            #[cfg(target_arch = "x86_64")]
            Kind::Sse41 => "sse4.1, four chunks at a time",
            #[cfg(target_arch = "x86_64")]
            Kind::Avx2 => "avx2, eight chunks at a time",
        }
    }

    /// how many chunks or parents one call compresses side by side
    #[inline]
    fn lanes(self) -> usize {
        match self.0 {
            Kind::Portable => 1,
            #[cfg(target_arch = "aarch64")]
            Kind::Neon => 4,
            #[cfg(target_arch = "aarch64")]
            Kind::NeonPaired => 8,
            #[cfg(target_arch = "x86_64")]
            Kind::Sse41 => 4,
            #[cfg(target_arch = "x86_64")]
            Kind::Avx2 => 8,
        }
    }

    /// the chaining value that follows `cv` after one block
    #[inline]
    fn compress(
        self,
        cv: &[u32; 8],
        block: &[u8; 64],
        block_len: u32,
        counter: u64,
        flags: u32,
    ) -> [u32; 8] {
        match self.0 {
            #[cfg(target_arch = "x86_64")]
            // each value only exists after its features were detected
            Kind::Sse41 => unsafe { sse41::compress(cv, block, block_len, counter, flags) },
            #[cfg(target_arch = "x86_64")]
            Kind::Avx2 => unsafe { sse41::compress_vex(cv, block, block_len, counter, flags) },
            _ => portable::compress(cv, block, block_len, counter, flags),
        }
    }

    /// advances `cv` over whole blocks of one chunk; the first one also gets `flags_start`
    #[inline]
    fn absorb(
        self,
        cv: &mut [u32; 8],
        blocks: &[[u8; 64]],
        counter: u64,
        flags: u32,
        flags_start: u32,
    ) {
        match self.0 {
            #[cfg(target_arch = "x86_64")]
            // each value only exists after its features were detected
            Kind::Sse41 => unsafe { sse41::absorb(cv, blocks, counter, flags, flags_start) },
            #[cfg(target_arch = "x86_64")]
            Kind::Avx2 => unsafe { sse41::absorb_vex(cv, blocks, counter, flags, flags_start) },
            _ => portable::absorb(cv, blocks, counter, flags, flags_start),
        }
    }

    /// Chaining values of the nodes in `input`, 32 bytes each, written to `out`;
    /// the first node counts `counter`.
    fn hash_many(self, batch: &lanes::Batch, input: &[u8], counter: u64, out: &mut [u8]) {
        assert!(out.len() >= batch.nodes(input.len()) * 32);
        // each value only exists after the features of its functions were detected
        let (rest, counter, out) = match self.0 {
            Kind::Portable => (input, counter, out),
            #[cfg(target_arch = "aarch64")]
            Kind::Neon => unsafe { neon::hash_many(batch, input, counter, out) },
            #[cfg(target_arch = "aarch64")]
            Kind::NeonPaired => unsafe { neon::hash_many_paired(batch, input, counter, out) },
            #[cfg(target_arch = "x86_64")]
            Kind::Sse41 => unsafe { sse41::hash_many(batch, input, counter, out) },
            #[cfg(target_arch = "x86_64")]
            Kind::Avx2 => unsafe { avx2::hash_many(batch, input, counter, out) },
        };
        lanes::one_by_one(batch, rest, counter, out, self);
    }
}

/// which implementation runs on this CPU
pub fn implementation() -> &'static str {
    Backend::detect().name()
}

fn key_words(key: &[u8; KEY_LEN]) -> [u32; 8] {
    let mut words = [0; 8];
    for (word, bytes) in words.iter_mut().zip(key.as_chunks::<4>().0) {
        *word = u32::from_le_bytes(*bytes);
    }
    words
}

/// BLAKE3 of one buffer
#[inline]
pub fn hash(input: &[u8]) -> Hash {
    hash_with(Backend::detect(), input)
}

/// BLAKE3 of one buffer, computed on several threads when the buffer is large enough
/// for that to pay: the same value as [`hash`].
pub fn hash_threads(input: &[u8]) -> Hash {
    static DEPTH: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    // two leaves of the split per processor, so that an uneven one does not idle the rest
    let depth = *DEPTH.get_or_init(|| {
        std::thread::available_parallelism().map_or(0, |count| count.get().ilog2() + 1)
    });
    tree::root_threads(input, &IV, 0, Backend::detect(), depth)
}

/// BLAKE3 of one buffer under a 32-byte key
#[inline]
pub fn keyed_hash(key: &[u8; KEY_LEN], input: &[u8]) -> Hash {
    keyed_hash_with(Backend::detect(), key, input)
}

/// A key derived from `key_material` for the purpose named by `context`, which
/// should be hardcoded, globally unique and application-specific.
pub fn derive_key(context: &str, key_material: &[u8]) -> [u8; OUT_LEN] {
    derive_key_with(Backend::detect(), context, key_material)
}

/// `hash` by a chosen implementation
pub fn hash_with(backend: Backend, input: &[u8]) -> Hash {
    tree::root(input, &IV, 0, backend)
}

/// `keyed_hash` by a chosen implementation
pub fn keyed_hash_with(backend: Backend, key: &[u8; KEY_LEN], input: &[u8]) -> Hash {
    tree::root(input, &key_words(key), KEYED_HASH, backend)
}

/// `derive_key` by a chosen implementation
pub fn derive_key_with(backend: Backend, context: &str, key_material: &[u8]) -> [u8; OUT_LEN] {
    let context_key = tree::root(context.as_bytes(), &IV, DERIVE_KEY_CONTEXT, backend);
    let key = key_words(context_key.as_bytes());
    *tree::root(key_material, &key, DERIVE_KEY_MATERIAL, backend).as_bytes()
}
