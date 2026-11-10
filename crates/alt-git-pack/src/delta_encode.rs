//! Bounded binary matching for the standard Git copy/insert delta format.
//! The index samples fixed source blocks; target matching slides by one byte,
//! so insertions/deletions do not require aligned edits. Hashes only select
//! candidates: every copied range is compared against the actual source.
use std::mem::size_of;

const BLOCK: usize = 16;
const SLOTS: usize = 4;
const MAX_BUCKETS: usize = 1 << 16;
const EMPTY: u32 = u32::MAX;
const MULTIPLIER: u64 = 257;
const POWER: u64 = MULTIPLIER.wrapping_pow((BLOCK - 1) as u32);

#[derive(Clone, Copy)]
struct Slot {
    fingerprint: u32,
    offset: u32,
}
type Bucket = [Slot; SLOTS];

pub(crate) struct Index {
    pub(crate) data: Box<[u8]>,
    buckets: Box<[Bucket]>,
}

/// A negative answer proves that no retained source anchor occurs in the
/// target. False positives only keep a candidate for the ordinary byte check.
/// This scratch allocation shares the window's memory budget.
pub(crate) struct TargetFilter {
    words: Box<[u64]>,
}

impl TargetFilter {
    pub(crate) fn new(target: &[u8], available: usize) -> Option<Self> {
        if target.len() < BLOCK {
            return None;
        }
        // 128 bits per target byte keeps unrelated anchors selective while
        // bounding initialization and memory independently of object limits.
        let words = target.len().checked_mul(2)?.checked_next_power_of_two()?;
        let bytes = words.checked_mul(size_of::<u64>())?;
        if bytes > available.min(1 << 20) {
            return None;
        }
        let mut filter = Self {
            words: vec![0; words].into_boxed_slice(),
        };
        let mut value = hash(&target[..BLOCK]);
        for position in 0..=target.len() - BLOCK {
            for bit in filter.positions((value >> 32) as u32) {
                filter.words[bit / 64] |= 1 << (bit % 64);
            }
            if let Some(next) = target.get(position + BLOCK) {
                value = value
                    .wrapping_sub((u64::from(target[position]) + 1).wrapping_mul(POWER))
                    .wrapping_mul(MULTIPLIER)
                    .wrapping_add(u64::from(*next) + 1);
            }
        }
        Some(filter)
    }

    fn positions(&self, fingerprint: u32) -> [usize; 3] {
        let mut value = u64::from(fingerprint);
        value = (value ^ (value >> 16)).wrapping_mul(0x9e3779b185ebca87);
        value ^= value >> 29;
        let step = (value >> 32) | 1;
        let mask = self.words.len() * 64 - 1;
        [
            value,
            value.wrapping_add(step),
            value.wrapping_add(step.wrapping_mul(2)),
        ]
        .map(|value| value as usize & mask)
    }

    fn contains(&self, fingerprint: u32) -> bool {
        self.positions(fingerprint)
            .into_iter()
            .all(|bit| self.words[bit / 64] & (1 << (bit % 64)) != 0)
    }
}

fn bucket_count(bytes: usize) -> usize {
    (bytes / BLOCK).clamp(1, MAX_BUCKETS).next_power_of_two()
}

fn hash(data: &[u8]) -> u64 {
    data.iter().fold(0u64, |value, byte| {
        value
            .wrapping_mul(MULTIPLIER)
            .wrapping_add(u64::from(*byte) + 1)
    })
}

impl Index {
    /// The optional precheck has a separate bounded slot allowance. Exhausting
    /// it leaves the candidate eligible; it cannot manufacture a negative.
    pub(crate) fn may_match(&self, target: &TargetFilter, remaining: &mut usize) -> bool {
        for bucket in &self.buckets {
            for slot in bucket {
                let Some(next) = remaining.checked_sub(1) else {
                    return true;
                };
                *remaining = next;
                if slot.offset != EMPTY && target.contains(slot.fingerprint) {
                    return true;
                }
            }
        }
        false
    }

    pub(crate) fn allocation(bytes: usize) -> usize {
        bytes + bucket_count(bytes) * size_of::<Bucket>()
    }

    pub(crate) fn new(data: &[u8]) -> Self {
        // Callers bound object size below the u32 offset range before building.
        debug_assert!(data.len() < u32::MAX as usize);
        let mut buckets = vec![
            [Slot {
                fingerprint: 0,
                offset: EMPTY
            }; SLOTS];
            bucket_count(data.len())
        ];
        let mask = buckets.len() - 1;
        // Cover the complete source within the fixed slot budget. Feeding
        // every block of a large object into last-four buckets evicts nearly
        // all early anchors, turning unchanged prefixes into long literals.
        // Leave half the slots available for hash collisions; matching still
        // scans every target byte and verifies the actual source bytes.
        let stride = BLOCK
            * (data.len() / BLOCK)
                .div_ceil(buckets.len() * SLOTS / 2)
                .max(1);
        for offset in (0..data.len().saturating_sub(BLOCK - 1)).step_by(stride) {
            let block = &data[offset..offset + BLOCK];
            let value = hash(block);
            let fingerprint = (value >> 32) as u32;
            let bucket = &mut buckets[value as usize & mask];
            // Keep the first occurrence of identical blocks. In particular a
            // run of equal bytes retains a base with useful following content.
            if bucket.iter().any(|slot| {
                slot.offset != EMPTY
                    && slot.fingerprint == fingerprint
                    && &data[slot.offset as usize..slot.offset as usize + BLOCK] == block
            }) {
                continue;
            }
            bucket.rotate_right(1);
            bucket[0] = Slot {
                fingerprint,
                offset: offset as u32,
            };
        }
        Self {
            data: data.into(),
            buckets: buckets.into_boxed_slice(),
        }
    }

    /// None means ordinary encoding should be used. The work budget is shared
    /// by all candidate attempts for one target; collisions cannot cause an
    /// unbounded search. The output limit also bounds unmatched target scans.
    pub(crate) fn encode(&self, target: &[u8], limit: usize, work: &mut u64) -> Option<Vec<u8>> {
        let mut output = Vec::new();
        size(&mut output, self.data.len());
        size(&mut output, target.len());
        if output.len() > limit {
            return None;
        }
        let mut position = 0;
        let mut literal = 0;
        let mut value = hash(target.get(..BLOCK).unwrap_or_default());
        while position + BLOCK <= target.len() {
            *work = work.checked_sub(1)?;
            let mut best = (0, 0);
            for slot in &self.buckets[value as usize & (self.buckets.len() - 1)] {
                if slot.offset == EMPTY || slot.fingerprint != (value >> 32) as u32 {
                    continue;
                }
                let offset = slot.offset as usize;
                let matched = prefix(&self.data[offset..], &target[position..], work)?;
                if matched >= BLOCK && matched > best.1 {
                    best = (offset, matched);
                }
            }
            if best.1 >= BLOCK {
                insert(&mut output, &target[literal..position], limit)?;
                copy(&mut output, best.0, best.1, limit)?;
                position += best.1;
                literal = position;
                if position + BLOCK <= target.len() {
                    *work = work.checked_sub(BLOCK as u64)?;
                    value = hash(&target[position..position + BLOCK]);
                }
            } else {
                if position + BLOCK < target.len() {
                    value = value
                        .wrapping_sub((u64::from(target[position]) + 1).wrapping_mul(POWER))
                        .wrapping_mul(MULTIPLIER)
                        .wrapping_add(u64::from(target[position + BLOCK]) + 1);
                }
                position += 1;
                // Flush bounded literals, rather than scanning an entire
                // unrelated object before discovering the output limit.
                if position - literal == 127 {
                    insert(&mut output, &target[literal..position], limit)?;
                    literal = position;
                }
            }
        }
        insert(&mut output, &target[literal..], limit)?;
        Some(output)
    }
}

fn prefix(base: &[u8], target: &[u8], work: &mut u64) -> Option<usize> {
    let available = base.len().min(target.len());
    let mut matched = 0;
    while matched < available {
        let count = (available - matched).min(32);
        // Charge both the block comparison and a possible mismatch search.
        *work = work.checked_sub((2 * count) as u64)?;
        let a = &base[matched..matched + count];
        let b = &target[matched..matched + count];
        if a != b {
            return Some(matched + a.iter().zip(b).position(|(a, b)| a != b).unwrap());
        }
        matched += count;
    }
    Some(matched)
}

fn size(output: &mut Vec<u8>, mut value: usize) {
    while value >= 128 {
        output.push(value as u8 | 0x80);
        value >>= 7;
    }
    output.push(value as u8);
}

fn insert(output: &mut Vec<u8>, bytes: &[u8], limit: usize) -> Option<()> {
    for chunk in bytes.chunks(127) {
        if output.len().checked_add(chunk.len() + 1)? > limit {
            return None;
        }
        output.push(chunk.len() as u8);
        output.extend_from_slice(chunk);
    }
    Some(())
}

fn copy(output: &mut Vec<u8>, mut offset: usize, mut count: usize, limit: usize) -> Option<()> {
    while count != 0 {
        let length = count.min(0xffffff);
        let mut operands = [0u8; 7];
        let mut used = 0;
        let mut opcode = 0x80;
        let encoded_length = if length == 0x10000 { 0 } else { length };
        for (index, byte) in (offset as u32)
            .to_le_bytes()
            .into_iter()
            .chain((encoded_length as u32).to_le_bytes().into_iter().take(3))
            .enumerate()
        {
            if byte != 0 {
                opcode |= 1 << index;
                operands[used] = byte;
                used += 1;
            }
        }
        if output.len().checked_add(used + 1)? > limit {
            return None;
        }
        output.push(opcode);
        output.extend_from_slice(&operands[..used]);
        count -= length;
        offset += length;
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::delta::apply;

    fn random(bytes: usize, mut state: u64) -> Vec<u8> {
        (0..bytes)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as u8
            })
            .collect()
    }

    #[test]
    fn arbitrary_shifted_edits_and_reordered_blocks_round_trip() {
        for seed in 1..=128 {
            let base = random(4096 + seed as usize, seed);
            let index = Index::new(&base);
            let mut target = b"an unaligned prefix".to_vec();
            target.extend_from_slice(&base[1021..2049]);
            target.extend(random(seed as usize, seed + 1));
            target.extend_from_slice(&base[..512]);
            target.extend_from_slice(&base[3071..]);
            let mut budget = 8 * target.len() as u64;
            let delta = index.encode(&target, target.len(), &mut budget).unwrap();
            assert_eq!(apply(&base, &delta).unwrap(), target);
        }
    }

    #[test]
    fn large_shifted_edits_fit_a_bounded_delta_without_losing_early_matches() {
        let base = random(32 << 20, 0x719af05);
        let index = Index::new(&base);
        let mut target = b"unaligned insertion".to_vec();
        target.extend_from_slice(&base);
        target[7_000_019..7_000_051].fill(91);
        target.drain(21_000_007..21_000_020);
        let mut work = 8 * target.len() as u64;
        let encoded = index
            .encode(&target, 8192, &mut work)
            .expect("small edits across a large source fit the output cap");
        assert_eq!(apply(&base, &encoded).unwrap(), target);
    }

    #[test]
    fn repeats_empty_small_and_binary_inputs_round_trip_or_fall_back() {
        for base in [vec![], vec![0], vec![255; 65536], random(2048, 5)] {
            let index = Index::new(&base);
            for target in [vec![], vec![0; 15], vec![255; 65537], base.clone()] {
                let mut budget = 8 * target.len() as u64 + 128;
                if let Some(delta) = index.encode(&target, target.len() + 1024, &mut budget) {
                    assert_eq!(apply(&base, &delta).unwrap(), target);
                }
            }
        }
    }

    #[test]
    fn target_filter_keeps_unaligned_insertions_deletions_and_end_matches() {
        for seed in 1..32 {
            let base = random(4096, seed);
            let index = Index::new(&base);
            let mut target = random(seed as usize, seed + 91);
            target.extend_from_slice(&base[37..2001]);
            target.extend_from_slice(&base[2033..]);
            let filter = TargetFilter::new(&target, 1 << 20).unwrap();
            let mut slots = usize::MAX;
            assert!(index.may_match(&filter, &mut slots));
            let delta = index
                .encode(&target, target.len() / 2, &mut (8 * target.len() as u64))
                .unwrap();
            assert_eq!(apply(&base, &delta).unwrap(), target);

            // The sole common anchor ends exactly at the target boundary.
            let mut end = vec![0; 35];
            end.extend_from_slice(&base[16..32]);
            let filter = TargetFilter::new(&end, 1 << 20).unwrap();
            assert!(index.may_match(&filter, &mut slots));
        }
    }

    #[test]
    fn negative_filter_is_a_proof_and_exhausted_bounds_fall_back() {
        let index = Index::new(&[0; 4096]);
        let target = [255; 4096];
        let filter = TargetFilter::new(&target, 1 << 20).unwrap();
        let mut slots = usize::MAX;
        let mut work = u64::MAX;
        assert!(!index.may_match(&filter, &mut slots));
        assert!(index.encode(&target, 2048, &mut work).is_none());
        assert!(index.may_match(&filter, &mut 0));
        assert!(TargetFilter::new(&target, 65535).is_none());
        assert!(TargetFilter::new(&target, 65536).is_some());
        assert!(TargetFilter::new(&vec![0; 65537], usize::MAX).is_none());
        assert!(TargetFilter::new(&[0; 15], usize::MAX).is_none());
    }

    #[test]
    fn output_and_search_bounds_stop_without_partial_delta() {
        let base = random(32768, 9);
        let index = Index::new(&base);
        assert!(index.encode(&base, base.len(), &mut 0).is_none());
        let mut work = u64::MAX;
        assert!(index.encode(&base, 1, &mut work).is_none());
        assert!(index.encode(&random(32768, 17), 64, &mut work).is_none());
        assert_eq!(
            index.data.len() + index.buckets.len() * size_of::<Bucket>(),
            Index::allocation(base.len())
        );
    }

    #[test]
    fn copy_encoding_covers_size_sentinel_and_three_byte_splits() {
        for length in [0x10000, 0xffffff, 0x1000000] {
            let base = vec![29; length + 0x123];
            let mut encoded = Vec::new();
            size(&mut encoded, base.len());
            size(&mut encoded, length);
            copy(&mut encoded, 0x123, length, 32).unwrap();
            assert_eq!(apply(&base, &encoded).unwrap(), base[0x123..]);
        }
    }
}
