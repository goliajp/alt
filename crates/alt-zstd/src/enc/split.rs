//! Where to end a block early. Entropy tables are per block, so a block
//! whose statistics shift part way through codes better as two.

use crate::frame::BLOCK_MAX;

/// Counts of a sample of a block's bytes.
struct Sample {
    counts: [u32; 256],
    total: u64,
}

impl Sample {
    /// Every `step`-th byte of `bytes`.
    fn of(bytes: &[u8], step: usize) -> Self {
        let mut counts = [0u32; 256];
        for &byte in bytes.iter().step_by(step) {
            counts[byte as usize] += 1;
        }
        Self {
            counts,
            total: bytes.len().div_ceil(step) as u64,
        }
    }

    /// How far apart two samples are, scaled by both totals: zero for equal
    /// distributions, twice the product of the totals for disjoint ones.
    fn distance(&self, other: &Self) -> u64 {
        let pairs = self.counts.iter().zip(&other.counts);
        pairs
            .map(|(&a, &b)| (a as u64 * other.total).abs_diff(b as u64 * self.total))
            .sum()
    }

    /// Whether `other` differs enough to deserve tables of its own. The
    /// tolerance grows by a sixteenth of the scale per point of `penalty`.
    fn differs(&self, other: &Self, penalty: u64) -> bool {
        self.distance(other) >= self.total * other.total * (14 + penalty) / 16
    }

    fn absorb(&mut self, other: &Self) {
        for (count, &more) in self.counts.iter_mut().zip(&other.counts) {
            *count += more;
        }
        self.total += other.total;
    }
}

/// Length of the first block to cut from a full `block`, judged from three
/// short samples: the cheap variant, for the fast strength.
pub(crate) fn by_borders(block: &[u8]) -> usize {
    const SEGMENT: usize = 512;
    let first = Sample::of(&block[..SEGMENT], 1);
    let last = Sample::of(&block[BLOCK_MAX - SEGMENT..], 1);
    if !first.differs(&last, 0) {
        return BLOCK_MAX;
    }
    let middle = BLOCK_MAX / 2 - SEGMENT / 2;
    let middle = Sample::of(&block[middle..middle + SEGMENT], 1);
    let (from_first, from_last) = (first.distance(&middle), last.distance(&middle));
    // the middle resembles neither end much more than the other: cut there
    if from_first.abs_diff(from_last) < (SEGMENT * SEGMENT / 3) as u64 {
        BLOCK_MAX / 2
    } else if from_first > from_last {
        BLOCK_MAX / 4
    } else {
        BLOCK_MAX / 4 * 3
    }
}

/// Length of the first block to cut from a full `block`: the block is
/// walked in chunks, and it ends before the first chunk that differs from
/// all before it. The variant for the default strength.
pub(crate) fn by_chunks(block: &[u8]) -> usize {
    const CHUNK: usize = 8 << 10;
    const STEP: usize = 43;
    // the last byte of a chunk is left out of its sample
    let sample = |at: usize| Sample::of(&block[at..at + CHUNK - 1], STEP);
    let mut past = sample(0);
    let mut penalty = 3;
    for at in (CHUNK..BLOCK_MAX).step_by(CHUNK) {
        let chunk = sample(at);
        if past.differs(&chunk, penalty) {
            return at;
        }
        past.absorb(&chunk);
        penalty = penalty.saturating_sub(1);
    }
    BLOCK_MAX
}
