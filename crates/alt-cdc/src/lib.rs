//! FastCDC content-defined chunking (Xia et al., 2016): gear rolling hash
//! with normalized chunking — a stricter mask before the average size and a
//! laxer one after, so chunk sizes cluster around the average while cut
//! points stay content-defined (shift-resistant).
//!
//! Business-agnostic stone. The gear table derives from a fixed seed and is
//! part of the on-disk chunking contract: changing it never breaks
//! correctness (addressing is content-based) but loses cross-version dedup,
//! so it is frozen.

/// Chunking bounds. The defaults are the M2 starting point; the store
/// records its parameters, so later tuning never breaks existing data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Params {
    pub min: usize,
    pub avg: usize,
    pub max: usize,
}

pub const DEFAULT_PARAMS: Params = Params {
    min: 16 * 1024,
    avg: 64 * 1024,
    max: 256 * 1024,
};

impl Params {
    /// Mask with `bits` high bits set; the gear hash accumulates history in
    /// its high bits, so cut-point tests use the top of the word.
    const fn mask(bits: u32) -> u64 {
        u64::MAX << (64 - bits)
    }

    const fn avg_bits(&self) -> u32 {
        self.avg.trailing_zeros()
    }

    /// Stricter mask before the average point (normalization level 2).
    const fn mask_strict(&self) -> u64 {
        Self::mask(self.avg_bits() + 2)
    }

    /// Laxer mask after the average point.
    const fn mask_lax(&self) -> u64 {
        Self::mask(self.avg_bits() - 2)
    }

    fn validate(&self) {
        assert!(self.min >= 64, "min chunk size too small");
        assert!(
            self.min < self.avg && self.avg < self.max,
            "min < avg < max"
        );
        assert!(self.avg.is_power_of_two(), "avg must be a power of two");
    }
}

/// The gear table, generated from a frozen seed via splitmix64.
/// Part of the chunking contract — never change.
const GEAR_SEED: u64 = 0x616c_745f_6364_6331; // "alt_cdc1"

const GEAR: [u64; 256] = {
    let mut table = [0u64; 256];
    let mut state = GEAR_SEED;
    let mut i = 0;
    while i < 256 {
        // splitmix64
        state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        table[i] = z ^ (z >> 31);
        i += 1;
    }
    table
};

/// Returns the length of the next chunk at the start of `data`
/// (`min..=max`, or all of `data` if shorter than `min`).
pub fn next_cut(data: &[u8], params: Params) -> usize {
    params.validate();
    if data.len() <= params.min {
        return data.len();
    }
    let cap = data.len().min(params.max);
    let normal = params.avg.min(cap);

    let mut fp: u64 = 0;
    let mut i = params.min;
    while i < normal {
        fp = (fp << 1).wrapping_add(GEAR[data[i] as usize]);
        if fp & params.mask_strict() == 0 {
            return i + 1;
        }
        i += 1;
    }
    while i < cap {
        fp = (fp << 1).wrapping_add(GEAR[data[i] as usize]);
        if fp & params.mask_lax() == 0 {
            return i + 1;
        }
        i += 1;
    }
    cap
}

/// Iterator over the chunk slices of `data`.
pub fn chunks(data: &[u8], params: Params) -> impl Iterator<Item = &[u8]> {
    let mut rest = data;
    core::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let cut = next_cut(rest, params);
        let (chunk, tail) = rest.split_at(cut);
        rest = tail;
        Some(chunk)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic pseudo-random bytes (splitmix64 stream).
    fn random_bytes(len: usize, seed: u64) -> Vec<u8> {
        let mut out = Vec::with_capacity(len);
        let mut state = seed;
        while out.len() < len {
            state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            out.extend_from_slice(&(z ^ (z >> 31)).to_le_bytes());
        }
        out.truncate(len);
        out
    }

    #[test]
    fn covers_input_exactly_and_respects_bounds() {
        let data = random_bytes(4 << 20, 1);
        let parts: Vec<&[u8]> = chunks(&data, DEFAULT_PARAMS).collect();
        let rejoined: Vec<u8> = parts.concat();
        assert_eq!(rejoined, data, "chunks must reassemble the input");
        for (i, part) in parts.iter().enumerate() {
            assert!(part.len() <= DEFAULT_PARAMS.max);
            if i + 1 != parts.len() {
                assert!(
                    part.len() >= DEFAULT_PARAMS.min,
                    "non-final chunk under min"
                );
            }
        }
    }

    #[test]
    fn deterministic_and_average_in_range() {
        let data = random_bytes(8 << 20, 2);
        let a: Vec<usize> = chunks(&data, DEFAULT_PARAMS).map(|c| c.len()).collect();
        let b: Vec<usize> = chunks(&data, DEFAULT_PARAMS).map(|c| c.len()).collect();
        assert_eq!(a, b);
        let mean = data.len() / a.len();
        assert!(
            (DEFAULT_PARAMS.avg / 3..=DEFAULT_PARAMS.avg * 3).contains(&mean),
            "mean chunk size {mean} too far from target {}",
            DEFAULT_PARAMS.avg
        );
    }

    #[test]
    fn shift_resistance() {
        // inserting one byte near the front must leave most downstream cut
        // points intact — the entire point of content-defined chunking
        let data = random_bytes(4 << 20, 3);
        let mut shifted = data.clone();
        shifted.insert(100, 0xAB);

        let offsets = |d: &[u8]| -> Vec<usize> {
            let mut at = 0;
            chunks(d, DEFAULT_PARAMS)
                .map(|c| {
                    at += c.len();
                    at
                })
                .collect()
        };
        let base: std::collections::HashSet<usize> = offsets(&data).into_iter().collect();
        let moved: Vec<usize> = offsets(&shifted).into_iter().map(|o| o - 1).collect();
        // compare in the shifted frame (everything after the insert moves by 1)
        let shared = moved.iter().filter(|o| base.contains(o)).count();
        let total = moved.len();
        assert!(
            shared * 10 >= total * 9,
            "only {shared}/{total} cut points survived a 1-byte insert"
        );
    }

    #[test]
    fn small_inputs() {
        assert_eq!(chunks(b"", DEFAULT_PARAMS).count(), 0);
        let tiny = random_bytes(1000, 4);
        let parts: Vec<&[u8]> = chunks(&tiny, DEFAULT_PARAMS).collect();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0], &tiny[..]);
    }

    #[test]
    fn gear_table_is_frozen() {
        // canary: if the seed or generator ever changes, this fails loudly —
        // cross-version dedup depends on the table being eternal.
        // Values independently computed with a reference splitmix64.
        assert_eq!(GEAR[0], 0xdd97_d862_b0cf_28ec);
        assert_eq!(GEAR[255], 0x128a_fc71_9cb3_c413);
    }
}
