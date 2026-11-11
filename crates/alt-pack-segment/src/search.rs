//! Search among records ordered by object id that share the first byte of the id.
//!
//! Ids are hashes, so within such a run they lie about evenly by the bytes that
//! follow. Halving the run reads a record in a different place every step; aiming
//! where those bytes say, measured from the nearest record already read, comes
//! within a record or two of the target after three reads, and the rest of the
//! search stays next to it.

use crate::Oid;
use std::cmp::Ordering;

/// reads that aim by the id's value before the search narrows step by step
const AIMED: usize = 3;

fn tail(id: &[u8]) -> u64 {
    u64::from_be_bytes(id[1..9].try_into().unwrap())
}

/// The index in `low..high` of the record whose id is `oid`. `id_at` gives the id
/// of a record; the ids in the range ascend and start with the first byte of `oid`.
/// Ids that are not spread evenly cost more reads, never a wrong answer.
pub(crate) fn find<'a>(
    mut low: usize,
    mut high: usize,
    oid: &Oid,
    id_at: impl Fn(usize) -> &'a [u8],
) -> Option<usize> {
    if low >= high {
        return None;
    }
    let key = tail(oid);
    // how far apart in value neighbours are on average
    let apart = (u64::MAX / (high - low) as u64).max(1);
    let mut at = low + (key / apart) as usize;
    // whether the last record read was below the target, which is then close above `low`
    let mut below = false;
    for _ in 0..AIMED {
        if low >= high {
            return None;
        }
        at = at.clamp(low, high - 1);
        let found = id_at(at);
        match found.cmp(oid.as_slice()) {
            Ordering::Equal => return Some(at),
            Ordering::Less => {
                low = at + 1;
                at = low.saturating_add((key.saturating_sub(tail(found)) / apart) as usize);
                below = true;
            }
            Ordering::Greater => {
                high = at;
                at = at.saturating_sub(1 + (tail(found).saturating_sub(key) / apart) as usize);
                below = false;
            }
        }
    }
    // steps that double away from the near end until the target is passed
    let mut step = 1;
    while high - low > step {
        let at = if below { low + step - 1 } else { high - step };
        match id_at(at).cmp(oid.as_slice()) {
            Ordering::Equal => return Some(at),
            Ordering::Less => {
                low = at + 1;
                if !below {
                    break;
                }
            }
            Ordering::Greater => {
                high = at;
                if below {
                    break;
                }
            }
        }
        step *= 2;
    }
    while low < high {
        let middle = low + (high - low) / 2;
        match id_at(middle).cmp(oid.as_slice()) {
            Ordering::Less => low = middle + 1,
            Ordering::Greater => high = middle,
            Ordering::Equal => return Some(middle),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn noise(state: &mut u64) -> u64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        *state
    }

    /// Distinct ids in order that all start with `first`. `shape` 0 spreads them
    /// evenly, 1 crowds most of them into a sliver of the range, 2 puts them all at
    /// its top and 3 makes them differ only in their last bytes.
    fn run(count: usize, first: u8, shape: u8, state: &mut u64) -> Vec<Oid> {
        let mut all: Vec<Oid> = (0..count)
            .map(|_| {
                let mut oid = [0u8; 20];
                for eight in oid.chunks_mut(8) {
                    let word = noise(state).to_le_bytes();
                    eight.copy_from_slice(&word[..eight.len()]);
                }
                oid[0] = first;
                match shape {
                    1 if oid[19] % 8 != 0 => oid[1..6].copy_from_slice(&[0x40, 0, 0, 0, 1]),
                    2 => oid[1..4].copy_from_slice(&[0xff, 0xff, 0xff]),
                    3 => oid[1..17].copy_from_slice(&[0x77; 16]),
                    _ => {}
                }
                oid
            })
            .collect();
        all.sort_unstable();
        all.dedup();
        all
    }

    #[test]
    fn finds_what_halving_finds_whatever_the_spread() {
        let mut state = 0x1319_8a2e_0370_7344;
        for shape in 0..4 {
            for count in [0, 1, 2, 3, 4, 5, 8, 9, 100, 1000, 40_000] {
                let all = run(count, 0x3c, shape, &mut state);
                // a run in the middle of other records, as in a segment
                let before = 17;
                let id_at = |index: usize| all[index - before].as_slice();
                let find = |oid: &Oid| find(before, before + all.len(), oid, id_at);
                for (index, oid) in all.iter().enumerate() {
                    assert_eq!(
                        find(oid),
                        Some(before + index),
                        "shape {shape} count {count}"
                    );
                    for step in [1u8, 0xff] {
                        let mut near = *oid;
                        near[19] = near[19].wrapping_add(step);
                        let expected = all.binary_search(&near).ok().map(|at| before + at);
                        assert_eq!(find(&near), expected, "shape {shape} count {count}");
                    }
                }
                for _ in 0..2000 {
                    let absent = run(1, 0x3c, shape % 2, &mut state)[0];
                    let expected = all.binary_search(&absent).ok().map(|at| before + at);
                    assert_eq!(find(&absent), expected);
                }
                let mut edge = [0u8; 20];
                edge[0] = 0x3c;
                assert_eq!(find(&edge), None);
                edge[1..].fill(0xff);
                assert_eq!(find(&edge), None);
            }
        }
    }

    #[test]
    fn reads_stay_few_when_ids_are_not_spread_evenly() {
        let mut state = 0xa409_3822_299f_31d0;
        for shape in 0..4 {
            let all = run(40_000, 0x3c, shape, &mut state);
            let reads = Cell::new(0usize);
            let id_at = |index: usize| {
                reads.set(reads.get() + 1);
                all[index].as_slice()
            };
            let mut most = 0;
            for oid in &all {
                reads.set(0);
                assert!(find(0, all.len(), oid, id_at).is_some());
                most = most.max(reads.get());
            }
            // the aimed reads, then doubling steps and halving, each at most the
            // logarithm of the run
            assert!(most <= AIMED + 2 * 17, "shape {shape}: {most} reads");
        }
    }
}
