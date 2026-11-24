//! A pack whose chains go on beyond what deltas may lie below an object is
//! refused where that is known, not when its last delta is resolved.

use super::readable::{Of, pack, read_back};
use super::waiting::BYTES;
use alt_pack_segment::{Error, Limits};

const DEPTH: u16 = 20;
const LINKS: usize = 100;

/// A plain object and a chain of `LINKS` deltas below it, each of the entry
/// before it; and the objects `again` of the chain once more, plain, behind
/// the chain.
fn chain(again: &[usize]) -> Vec<(usize, Of)> {
    let mut entries = vec![(0, Of::Nothing)];
    entries.extend((1..=LINKS).map(|n| (n, Of::Entry(n - 1))));
    entries.extend(again.iter().map(|n| (*n, Of::Nothing)));
    entries
}

/// What the entries inflate to as they are scanned, and `made` objects beside.
fn limits(entries: &[(usize, Of)], made: usize) -> Limits {
    let scanned: usize = (entries.iter())
        .map(|(_, of)| if matches!(of, Of::Nothing) { BYTES } else { 18 })
        .sum();
    Limits {
        delta_depth: DEPTH,
        ..Limits::of(1 << 20, 1 << 20, (scanned + made * BYTES) as u64)
    }
}

/// The chain alone lies a hundred deltas deep where twenty may. It is refused
/// for that with thirty of its objects made at the most: resolved to its end
/// it would have been refused for what it inflates to.
#[test]
fn a_chain_beyond_the_depth_is_refused_before_it_is_resolved_to_its_end() {
    let entries = chain(&[]);
    let refused = read_back(&pack(&entries), &limits(&entries, 30), &[]);
    assert!(
        matches!(refused, Err(Error::Limit("delta depth"))),
        "{:?}",
        refused.map(|index| index.len())
    );
    // as it was, with room for every object
    let refused = read_back(&pack(&entries), &limits(&entries, LINKS), &[]);
    assert!(matches!(refused, Err(Error::Limit("delta depth"))));
}

/// The same chain with every fifteenth object in the pack once more, whole: no
/// object lies more than fourteen deltas below a plain entry, the pack is taken
/// in, and every object is read. With the fifteenth alone once more the rest of
/// the chain lies eighty-five deep, and the pack is refused: what the walks
/// left beyond the depth is resolved before that is said.
#[test]
fn a_chain_beyond_the_depth_whose_objects_are_had_by_shorter_ways_is_taken_in() {
    let again: Vec<usize> = (15..=LINKS).step_by(15).collect();
    let entries = chain(&again);
    let objects: Vec<usize> = (0..=LINKS).collect();
    read_back(&pack(&entries), &limits(&entries, 2 * LINKS), &objects).unwrap();
    let entries = chain(&[15]);
    let refused = read_back(&pack(&entries), &limits(&entries, 2 * LINKS), &[]);
    assert!(
        matches!(refused, Err(Error::Limit("delta depth"))),
        "{:?}",
        refused.map(|index| index.len())
    );
}
