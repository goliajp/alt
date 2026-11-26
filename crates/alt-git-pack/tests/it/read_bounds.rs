//! What a read of an indexed pack takes is bounded by the entries of the
//! pack, however often the pack holds an object and whatever its copies are
//! deltas of: the entries looked at, the entries of ids gone through, the
//! deltas applied and the bytes held.

use alt_git_codec::{HashAlgo, ObjectId};
use alt_git_pack::{HELD_FOR_AN_ENTRY, IndexedPack, MAX_DELTA_DEPTH, ReadWork, index_pack};

use crate::held::{ended_within, most_held, no_more_than};
use crate::index_pack::{blob, delta, object, ofs_entry, plain_entry, ref_entry, written};

/// what a read holds beside what it holds for each entry: the objects of
/// these packs are of a few bytes, and the cache keeps those of one way
const BESIDE: usize = 1 << 20;

/// A delta that makes object `n` of object `base`, copying what they share.
fn of(base: usize, n: usize) -> Vec<u8> {
    let (from, to) = (object(base), object(n));
    let keep = if base == n { to.len() } else { 18 };
    delta(from.len(), keep, &to)
}

/// Reads the entry `i` of the index by where it is and by its id, each from
/// a pack opened anew, and holds each read to the bounds.
fn bounded(path: &std::path::Path, i: u32) {
    let listed = IndexedPack::open(path, HashAlgo::Sha1).unwrap();
    let entries = listed.idx().len() as usize;
    let (oid, offset) = (listed.idx().oid_at(i), listed.idx().offset_at(i).unwrap());
    for by_id in [false, true] {
        let cold = IndexedPack::open(path, HashAlgo::Sha1).unwrap();
        let mut work = ReadWork::default();
        // far beyond the bound, and far below what a read without one took
        let _held = no_more_than(64 * (HELD_FOR_AN_ENTRY * entries + BESIDE));
        let (held, read) = most_held(|| match by_id {
            true => cold.read_counting(&oid, &mut work).unwrap().unwrap(),
            false => cold.read_at_counting(offset, &mut work).unwrap(),
        });
        assert_eq!(
            ObjectId::hash_object(HashAlgo::Sha1, read.kind, &read.data),
            oid
        );
        assert!(work.looked <= entries, "{work:?} of {entries}");
        assert!(work.ranged <= entries, "{work:?} of {entries}");
        assert!(work.applied <= MAX_DELTA_DEPTH as usize, "{work:?}");
        let may = HELD_FOR_AN_ENTRY * work.looked + BESIDE;
        assert!(
            held <= may,
            "{held} bytes held, {may} at the most: {work:?}"
        );
    }
}

/// An object stored once, many times a delta of that entry by where it is,
/// and many times a delta of itself by its id: every delta by id comes to
/// every entry of the object. A read once held a list of them all for each.
#[test]
fn an_object_a_pack_holds_thousands_of_times_is_read_within_what_its_entries_bound() {
    let _ended = ended_within(900);
    let (by_id, by_place) = (1000, 10_000);
    let x = object(0);
    let mut entries = vec![plain_entry(&x)];
    let mut at = entries[0].len();
    for _ in 0..by_place {
        let entry = ofs_entry(at, &of(0, 0));
        at += entry.len();
        entries.push(entry);
    }
    entries.extend((0..by_id).map(|_| ref_entry(&blob(&x), &of(0, 0))));
    let dir = alt_os::temp::tempdir().unwrap();
    let path = written(&dir, &entries);
    index_pack(&path, HashAlgo::Sha1, false).unwrap();
    let count = entries.len() as u32;
    for i in (0..count).step_by(count as usize / 40).chain([count - 1]) {
        bounded(&path, i);
    }
}

/// The same pack read as one who lists a pack reads it: every entry, one
/// after the other, from the pack opened once. The entries of the object are
/// gone through by the first read that comes to its id, and each delta by id
/// read after that comes to the one entry it was found in. Each of them once
/// went through them all: a thousand times eleven thousand.
#[test]
fn the_entries_of_an_object_are_gone_through_once_for_all_the_deltas_made_of_it_by_id() {
    let _ended = ended_within(900);
    let (by_id, by_place) = (1000, 10_000);
    let x = object(0);
    let mut entries = vec![plain_entry(&x)];
    let mut at = entries[0].len();
    for _ in 0..by_place {
        let entry = ofs_entry(at, &of(0, 0));
        at += entry.len();
        entries.push(entry);
    }
    entries.extend((0..by_id).map(|_| ref_entry(&blob(&x), &of(0, 0))));
    let dir = alt_os::temp::tempdir().unwrap();
    let path = written(&dir, &entries);
    index_pack(&path, HashAlgo::Sha1, false).unwrap();
    let pack = IndexedPack::open(&path, HashAlgo::Sha1).unwrap();
    let mut work = ReadWork::default();
    for i in 0..entries.len() as u32 {
        let (oid, offset) = (pack.idx().oid_at(i), pack.idx().offset_at(i).unwrap());
        let read = pack.read_at_counting(offset, &mut work).unwrap();
        assert_eq!(
            ObjectId::hash_object(HashAlgo::Sha1, read.kind, &read.data),
            oid
        );
    }
    assert!(work.ranged <= 2 * entries.len(), "{work:?}");
    // and by its id, which is that of them all
    let before = work.ranged;
    pack.read_counting(&blob(&x), &mut work).unwrap().unwrap();
    assert_eq!(work.ranged, before + 1);
}

/// An object X stored once and ten thousand times a delta of that entry by
/// where it is; a thousand objects Z, each stored twice as a delta of X by its
/// id, and each once more a delta of itself by its id. Each read of that last
/// comes to Z twice, and to X under each: the second time X is under another
/// entry than the one it was found in, and it is no reason to go through all
/// the entries of X again. Every entry read from the pack opened once goes
/// through no more entries of ids than twice the entries there are, where it
/// once went through the entries of X for each Z.
#[test]
fn an_object_two_entries_of_another_are_made_of_is_gone_through_once_for_all_the_reads() {
    let _ended = ended_within(900);
    let (made, by_place) = (1000, 10_000);
    let x = object(0);
    let mut entries = vec![plain_entry(&x)];
    let mut at = entries[0].len();
    for _ in 0..by_place {
        let entry = ofs_entry(at, &of(0, 0));
        at += entry.len();
        entries.push(entry);
    }
    for z in 1..=made {
        entries.push(ref_entry(&blob(&x), &of(0, z)));
        entries.push(ref_entry(&blob(&x), &of(0, z)));
        entries.push(ref_entry(&blob(&object(z)), &of(z, z)));
    }
    let dir = alt_os::temp::tempdir().unwrap();
    let path = written(&dir, &entries);
    index_pack(&path, HashAlgo::Sha1, false).unwrap();
    let pack = IndexedPack::open(&path, HashAlgo::Sha1).unwrap();
    let mut work = ReadWork::default();
    for i in 0..entries.len() as u32 {
        let (oid, offset) = (pack.idx().oid_at(i), pack.idx().offset_at(i).unwrap());
        let read = pack.read_at_counting(offset, &mut work).unwrap();
        assert_eq!(
            ObjectId::hash_object(HashAlgo::Sha1, read.kind, &read.data),
            oid
        );
    }
    assert!(work.ranged <= 2 * entries.len(), "{work:?}");
}

/// Packs of thousands of entries made at random: of how many objects, how
/// often each is there again, and how long the chains are differ from pack
/// to pack. Those `index_pack` takes are read within the bounds.
#[test]
fn packs_of_thousands_of_entries_at_random_are_read_within_what_their_entries_bound() {
    let _ended = ended_within(1800);
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    let mut next = |below: usize| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 33) as usize % below
    };
    let (mut taken, mut again, mut deepest) = (0, 0, 0);
    for round in 0..40 {
        // In one kind of pack few objects make many copies of each, and a chain
        // goes on from the entry before it more often in some than in others.
        // In the other nearly every entry makes an object of its own of the
        // entry before it: the way down from its last entries is long.
        let long = round % 2 == 1;
        let most = 1 << (1 + next(11));
        let universe = 2 + next(most);
        let count = 1000 + next(3000);
        let (on, by_id) = (1 + next(40), 1 + next(6));
        let mut entries: Vec<Vec<u8>> = Vec::new();
        let mut made: Vec<(usize, usize)> = Vec::new();
        let mut at = 12;
        for index in 0..count {
            let entry = match (long, made.last().copied()) {
                (true, Some(last)) => {
                    let (n, way) = match next(20) {
                        0 => (made[next(made.len())].0, next(400)),
                        _ => (universe + index, next(400)),
                    };
                    made.push((n, at));
                    match way {
                        0 => plain_entry(&object(n)),
                        1 => {
                            let base = made[next(made.len() - 1)].0;
                            ref_entry(&blob(&object(base)), &of(base, n))
                        }
                        _ => ofs_entry(at - last.1, &of(last.0, n)),
                    }
                }
                (_, last) => {
                    let n = next(universe);
                    made.push((n, at));
                    match (next(on + by_id + 1), last) {
                        (0, _) | (_, None) => plain_entry(&object(n)),
                        (way, _) if way <= by_id => {
                            let base = next(universe);
                            ref_entry(&blob(&object(base)), &of(base, n))
                        }
                        (way, Some(last)) => {
                            let (base, began) = match way % 4 {
                                0 => made[next(made.len() - 1)],
                                _ => last,
                            };
                            ofs_entry(at - began, &of(base, n))
                        }
                    }
                }
            };
            at += entry.len();
            entries.push(entry);
        }
        let dir = alt_os::temp::tempdir().unwrap();
        let path = written(&dir, &entries);
        if index_pack(&path, HashAlgo::Sha1, false).is_err() {
            continue;
        }
        let mut objects: Vec<usize> = made.iter().map(|(n, _)| *n).collect();
        objects.sort_unstable();
        objects.dedup();
        taken += 1;
        again += usize::from(objects.len() * 2 < count);
        for _ in 0..24 {
            bounded(&path, next(count) as u32);
        }
        // how far down the entries of this pack are, by its last
        let last = IndexedPack::open(&path, HashAlgo::Sha1).unwrap();
        let mut work = ReadWork::default();
        last.read_at_counting(made[count - 1].1 as u64, &mut work)
            .unwrap();
        deepest = deepest.max(work.applied);
    }
    println!("taken, with objects many times over, the longest way: {taken}, {again}, {deepest}");
    assert!(
        taken >= 10 && again >= 5 && deepest >= 100,
        "{taken} {again} {deepest}"
    );
}
