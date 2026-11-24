//! Packs that hold an object more than once, one of its entries a delta of
//! the object itself or of what is made of it: what `index_pack` takes is
//! read, every entry of it, and no read goes round.

use alt_git_codec::{HashAlgo, ObjectId};
use alt_git_pack::{IndexedPack, index_pack};

use crate::index_pack::{blob, delta, object, ofs_entry, plain_entry, ref_entry, written};

/// what the objects here begin with alike
const ALIKE: usize = 18;

/// A delta that makes object `n` of object `base`: of itself by copying all
/// of it, of another by copying what they begin with alike.
fn of(base: usize, n: usize) -> Vec<u8> {
    let (from, to) = (object(base), object(n));
    let keep = if base == n { to.len() } else { ALIKE };
    delta(from.len(), keep, &to)
}

/// Reads every entry of the pack at `path` by where it is and by its id, each
/// from a pack opened anew so that nothing read before helps, and holds what
/// comes back against the id. A read looks at no entry twice.
fn all_read(path: &std::path::Path, entries: usize) {
    let listed = IndexedPack::open(path, HashAlgo::Sha1).unwrap();
    let idx = listed.idx();
    assert_eq!(idx.len() as usize, entries);
    for i in 0..idx.len() {
        let (oid, offset) = (idx.oid_at(i), idx.offset_at(i).unwrap());
        let is = |read: alt_git_pack::PackedObject| {
            assert_eq!(
                ObjectId::hash_object(HashAlgo::Sha1, read.kind, &read.data),
                oid
            );
        };
        let mut steps = 0;
        let cold = IndexedPack::open(path, HashAlgo::Sha1).unwrap();
        is(cold.read_at_counting(offset, &mut steps).unwrap());
        assert!(steps <= entries, "{steps} steps over {entries} entries");
        let cold = IndexedPack::open(path, HashAlgo::Sha1).unwrap();
        is(cold.read(&oid).unwrap().unwrap());
        is(listed.read_at(offset).unwrap());
    }
}

/// The three packs a read of did not come back from. An object stored, and
/// again as a delta of itself; an object stored, another made of it, and the
/// first again as a delta of the second, named by its id or by where it is.
#[test]
fn an_object_that_is_there_again_as_a_delta_of_itself_or_of_what_is_made_of_it_is_read() {
    let (x, y) = (object(0), object(1));
    let own = vec![plain_entry(&x), ref_entry(&blob(&x), &of(0, 0))];
    let second = ref_entry(&blob(&x), &of(0, 1));
    let by_id = vec![
        plain_entry(&x),
        second.clone(),
        ref_entry(&blob(&y), &of(1, 0)),
    ];
    let by_place = vec![
        plain_entry(&x),
        second.clone(),
        ofs_entry(second.len(), &of(1, 0)),
    ];
    for entries in [own, by_id, by_place] {
        let dir = alt_os::temp::tempdir().unwrap();
        let path = written(&dir, &entries);
        index_pack(&path, HashAlgo::Sha1, false).unwrap();
        all_read(&path, entries.len());
    }
}

/// Packs made at random of a few objects: an entry is an object stored, a
/// delta of any object by its id, the object itself among them, or a delta
/// of an entry before it. Those `index_pack` takes are read through.
#[test]
fn every_entry_of_a_pack_that_is_taken_is_read_whatever_it_is_a_delta_of() {
    let mut state = 0x2545_f491_4f6c_dd1d_u64;
    let mut next = |below: usize| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 33) as usize % below
    };
    let (mut taken, mut again, mut own) = (0, 0, 0);
    for _ in 0..2000 {
        let universe = 2 + next(5);
        let count = 2 + next(9);
        // each entry, the object it makes and where it begins
        let mut entries: Vec<Vec<u8>> = Vec::new();
        let mut made: Vec<(usize, usize)> = Vec::new();
        let (mut at, mut of_itself) = (12, false);
        for _ in 0..count {
            let n = next(universe);
            let entry = match (next(3), made.is_empty()) {
                (0, _) | (2, true) => plain_entry(&object(n)),
                (1, _) => {
                    let base = next(universe);
                    of_itself |= base == n;
                    ref_entry(&blob(&object(base)), &of(base, n))
                }
                _ => {
                    let (base, began) = made[next(made.len())];
                    ofs_entry(at - began, &of(base, n))
                }
            };
            made.push((n, at));
            at += entry.len();
            entries.push(entry);
        }
        let dir = alt_os::temp::tempdir().unwrap();
        let path = written(&dir, &entries);
        if index_pack(&path, HashAlgo::Sha1, false).is_err() {
            continue;
        }
        all_read(&path, count);
        let mut objects: Vec<usize> = made.iter().map(|(n, _)| *n).collect();
        objects.sort_unstable();
        objects.dedup();
        taken += 1;
        again += usize::from(objects.len() < count);
        own += usize::from(of_itself);
    }
    println!("taken, with an object again, with a delta of itself: {taken}, {again}, {own}");
    assert!(
        taken > 200 && again > 100 && own > 20,
        "{taken} {again} {own}"
    );
}
