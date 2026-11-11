//! The literals section of a compressed block.

use super::huf::{self, Codes};
use crate::huffman::MAX_BITS;

/// Huffman code of the last block that sent one, which later blocks may
/// reuse without describing it again.
pub(crate) struct LiteralCoder {
    codes: Codes,
    valid: bool,
    /// scratch for a tree description
    tree: Vec<u8>,
    /// scratch for the four literal streams
    streams: Vec<u8>,
}

/// Bytes inspected at each end of a block suspected to be incompressible.
const SAMPLE: usize = 4096;

/// Literals per sequence from which a block is suspected to be noise.
const SAMPLE_RUN: usize = 20;

/// Bytes a block must save for compression to be worth its decoding cost.
pub(crate) fn min_gain(len: usize) -> usize {
    (len >> 6) + 2
}

fn histogram(src: &[u8]) -> [u32; 256] {
    // which of the two is faster differs by architecture
    if cfg!(target_arch = "aarch64") {
        histogram_by_words(src)
    } else {
        histogram_by_bytes(src)
    }
}

fn histogram_by_bytes(src: &[u8]) -> [u32; 256] {
    // four tables keep consecutive equal bytes from waiting on each other
    let mut parts = [[0u32; 256]; 4];
    let (groups, rest) = src.as_chunks::<4>();
    for group in groups {
        parts[0][group[0] as usize] += 1;
        parts[1][group[1] as usize] += 1;
        parts[2][group[2] as usize] += 1;
        parts[3][group[3] as usize] += 1;
    }
    for &byte in rest {
        parts[0][byte as usize] += 1;
    }
    std::array::from_fn(|i| parts[0][i] + parts[1][i] + parts[2][i] + parts[3][i])
}

fn histogram_by_words(src: &[u8]) -> [u32; 256] {
    // a table per byte of the word, and one load feeds eight counts
    let mut parts = [[0u32; 256]; 8];
    let (words, rest) = src.as_chunks::<8>();
    for word in words {
        let word = u64::from_le_bytes(*word);
        for (lane, part) in parts.iter_mut().enumerate() {
            part[(word >> (8 * lane)) as u8 as usize] += 1;
        }
    }
    for &byte in rest {
        parts[0][byte as usize] += 1;
    }
    std::array::from_fn(|i| parts.iter().map(|part| part[i]).sum())
}

/// Section header for raw (kind 0) or single-byte (kind 1) literals.
fn plain_header(kind: u8, len: usize, out: &mut Vec<u8>) {
    if len < 32 {
        out.push(kind | (len as u8) << 3);
    } else if len < 4096 {
        out.extend_from_slice(&(kind as u16 | 1 << 2 | (len as u16) << 4).to_le_bytes());
    } else {
        let bits = kind as u32 | 3 << 2 | (len as u32) << 4;
        out.extend_from_slice(&bits.to_le_bytes()[..3]);
    }
}

fn raw(literals: &[u8], out: &mut Vec<u8>) {
    plain_header(0, literals.len(), out);
    out.extend_from_slice(literals);
}

impl LiteralCoder {
    pub(crate) fn new() -> Self {
        Self {
            codes: Codes::new(),
            valid: false,
            tree: Vec::new(),
            streams: Vec::new(),
        }
    }

    pub(crate) fn save(&self) -> (Codes, bool) {
        (self.codes.clone(), self.valid)
    }

    pub(crate) fn restore(&mut self, state: (Codes, bool)) {
        (self.codes, self.valid) = state;
    }

    pub(crate) fn reset(&mut self) {
        self.valid = false;
    }

    /// Appends the literals section for `literals`, which belong to a block
    /// with `sequences` sequences.
    pub(crate) fn encode(&mut self, literals: &[u8], sequences: usize, out: &mut Vec<u8>) {
        let len = literals.len();
        // a tree description does not pay off on a handful of bytes
        if len <= if self.valid { 6 } else { 63 } {
            return raw(literals, out);
        }
        // long literal runs between few matches are usually noise: look at
        // both ends before counting everything
        if len >= 2 * SAMPLE && len / SAMPLE_RUN >= sequences {
            let ends = [&literals[..SAMPLE], &literals[len - SAMPLE..]];
            let most: u32 = ends
                .iter()
                .map(|part| *histogram(part).iter().max().unwrap())
                .sum();
            if most as usize <= ((2 * SAMPLE) >> 7) + 4 {
                return raw(literals, out);
            }
        }
        let counts = histogram(literals);
        let most = *counts.iter().max().unwrap() as usize;
        if most == len {
            plain_header(1, len, out);
            out.push(literals[0]);
            return;
        }
        if most <= (len >> 7) + 4 {
            // close to uniform: nothing to gain
            return raw(literals, out);
        }
        let max_symbol = counts.iter().rposition(|&count| count != 0).unwrap();

        let limit = MAX_BITS
            .min((len as u32 - 1).ilog2() - 1)
            .max((len as u32).ilog2().min((max_symbol as u32).ilog2() + 1) + 1);
        let fresh = huf::build(&counts, limit.clamp(5, MAX_BITS));
        self.tree.clear();
        let fresh_bits = huf::write_tree(&fresh, max_symbol, &mut self.tree)
            .then(|| fresh.cost(&counts).unwrap() + 8 * self.tree.len() as u32);
        let kept_bits = if self.valid {
            self.codes.cost(&counts)
        } else {
            None
        };
        let reuse = match (kept_bits, fresh_bits) {
            (Some(kept), Some(fresh)) => kept <= fresh,
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (None, None) => return raw(literals, out),
        };
        let bits = if reuse { kept_bits } else { fresh_bits }.unwrap();
        let budget = len - min_gain(len);
        if bits as usize / 8 >= budget {
            return raw(literals, out);
        }

        let four = len >= 256;
        let header = match len {
            0..1024 => 3,
            1024..16384 => 4,
            _ => 5,
        };
        let start = out.len();
        out.extend_from_slice(&[0; 5][..header]);
        let codes = if reuse {
            &self.codes
        } else {
            out.extend_from_slice(&self.tree);
            &fresh
        };
        if four {
            huf::encode_4x(codes, literals, &mut self.streams, out);
        } else {
            huf::encode_stream(codes, literals, out);
        }
        let packed = out.len() - start - header;
        if packed >= budget {
            out.truncate(start);
            return raw(literals, out);
        }
        let kind = if reuse { 3 } else { 2 };
        let bits = match header {
            3 => kind | (four as u64) << 2 | (len as u64) << 4 | (packed as u64) << 14,
            4 => kind | 2 << 2 | (len as u64) << 4 | (packed as u64) << 18,
            _ => kind | 3 << 2 | (len as u64) << 4 | (packed as u64) << 22,
        };
        out[start..start + header].copy_from_slice(&bits.to_le_bytes()[..header]);
        if !reuse {
            self.codes = fresh;
            self.valid = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{histogram_by_bytes, histogram_by_words};

    #[test]
    fn both_histograms_count_alike() {
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let data: Vec<u8> = (0..5000)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                ((state >> 56) as u8).min((state >> 48) as u8)
            })
            .collect();
        for len in [0, 1, 7, 8, 9, 63, 64, 4099, 5000] {
            let part = &data[..len];
            assert!(
                histogram_by_bytes(part) == histogram_by_words(part),
                "{len} bytes"
            );
        }
    }
}
