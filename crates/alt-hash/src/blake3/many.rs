//! many independent inputs hashed in one call

use super::lanes::{Short, alike};
use super::{Backend, CHUNK_LEN, Hash, IV, MAX_LANES, hash_with};

/// BLAKE3 of each of `inputs`, in their order: the same values as [`super::hash`]
/// gives one by one. Inputs of at most a chunk are compressed several side by side,
/// which a single short input cannot be.
pub fn hash_many(inputs: &[&[u8]]) -> Vec<Hash> {
    hash_many_with(Backend::detect(), inputs)
}

/// `hash_many` by a chosen implementation
pub fn hash_many_with(backend: Backend, inputs: &[&[u8]]) -> Vec<Hash> {
    let mut out = vec![Hash::from_bytes([0; 32]); inputs.len()];
    let lanes = backend.ragged_lanes();
    // (blocks, index) of the inputs of one chunk
    let mut single = Vec::new();
    for (index, input) in inputs.iter().enumerate() {
        if lanes != 0 && input.len() <= CHUNK_LEN {
            single.push((input.len().div_ceil(64), index));
        } else {
            out[index] = hash_with(backend, input);
        }
    }
    // in order of length, so that those compressed together are alike
    single.sort_unstable();
    for group in single.chunks(lanes.max(1)) {
        let mut short = [Short {
            input: &[],
            counter: 0,
            root: true,
        }; MAX_LANES];
        for (short, (_, index)) in short.iter_mut().zip(group) {
            short.input = inputs[*index];
        }
        let short = &short[..group.len()];
        if alike(short) {
            let mut hashes = [0; MAX_LANES * 32];
            let hashes = &mut hashes[..group.len() * 32];
            backend.ragged(&IV, 0, short, hashes);
            for ((_, index), hash) in group.iter().zip(hashes.as_chunks::<32>().0) {
                out[*index] = Hash::from_bytes(*hash);
            }
        } else {
            for (_, index) in group {
                out[*index] = hash_with(backend, inputs[*index]);
            }
        }
    }
    out
}
