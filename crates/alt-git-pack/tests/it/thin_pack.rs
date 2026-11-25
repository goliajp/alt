//! `index_pack_thin` over packs as a push sends them: deltas of objects
//! the receiver has, which the pack leaves out.

use std::cell::Cell;

use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_git_pack::{IndexLimits, IndexedPack, PackError, index_pack_thin, index_pack_within};
use alt_os::temp::TempDir;

use crate::index_pack::{blob, by_git, delta, object, ofs_entry, plain_entry, ref_entry, written};

/// A store that holds the objects `held`, and counts what is asked of it.
fn store<'a>(
    held: &'a [Vec<u8>],
    asked: &'a Cell<usize>,
) -> impl Fn(&ObjectId) -> std::io::Result<Option<(ObjectKind, Vec<u8>)>> + 'a {
    move |oid| {
        asked.set(asked.get() + 1);
        let found = held.iter().find(|data| blob(data) == *oid);
        Ok(found.map(|data| (ObjectKind::Blob, data.clone())))
    }
}

/// Object 8 as a delta of object 7 by its id, object 9 as a delta of
/// that entry, object 6 as another delta of object 7, and a plain
/// object: neither 7 nor anything it is made of is in the pack.
fn thin() -> Vec<Vec<u8>> {
    let base = object(7);
    let first = ref_entry(&blob(&base), &delta(base.len(), 18, &object(8)));
    let second = ofs_entry(first.len(), &delta(object(8).len(), 18, &object(9)));
    let third = ref_entry(&blob(&base), &delta(base.len(), 18, &object(6)));
    vec![first, second, third, plain_entry(&object(0))]
}

#[test]
fn the_bases_a_pack_leaves_out_are_appended_from_the_store_it_was_sent_to() {
    let dir = TempDir::new().unwrap();
    let pack = written(&dir, &thin());
    let limits = IndexLimits::default();
    // as a pack that is to hold its bases it is refused
    let alone = index_pack_within(&pack, HashAlgo::Sha1, false, &limits);
    assert!(matches!(alone, Err(PackError::Format(_))), "{alone:?}");

    let (held, asked) = ([object(7), object(3)], Cell::new(0));
    let bases = store(&held, &asked);
    let indexed = index_pack_thin(&pack, HashAlgo::Sha1, false, &limits, Some(&bases)).unwrap();
    // the one base, asked for once, is the fifth object of the pack
    assert_eq!((indexed.objects, asked.get()), (5, 1));
    let whole = std::fs::read(&pack).unwrap();
    assert_eq!(whole[8..12], 5u32.to_be_bytes());
    assert_eq!(whole[whole.len() - 20..], indexed.trailer[..]);
    let read = IndexedPack::open(&pack, HashAlgo::Sha1).unwrap();
    for n in [0, 6, 7, 8, 9] {
        let data = read.read(&blob(&object(n))).unwrap().expect("in the pack");
        assert_eq!(*data.data, object(n));
    }
    // and Git reads the pack as it is now, to the same index
    assert_eq!(std::fs::read(&indexed.idx_path).unwrap(), by_git(&pack));
}

#[test]
fn a_base_the_store_lacks_too_leaves_the_pack_refused_and_what_is_appended_is_counted() {
    let dir = TempDir::new().unwrap();
    let pack = written(&dir, &thin());
    let (held, asked) = ([object(3)], Cell::new(0));
    let bases = store(&held, &asked);
    let limits = IndexLimits::default();
    let refused = index_pack_thin(&pack, HashAlgo::Sha1, false, &limits, Some(&bases));
    assert!(matches!(refused, Err(PackError::Format(_))), "{refused:?}");
    assert_eq!(asked.get(), 1);

    // the pack inflates to its four entries, the base and three objects
    let pack = written(&dir, &thin());
    let held = [object(7)];
    let bases = store(&held, &asked);
    let entries: usize = [8, 9, 6]
        .iter()
        .map(|n| delta(object(7).len(), 18, &object(*n)).len())
        .sum();
    let lengths = |of: &[usize]| of.iter().map(|n| object(*n).len()).sum::<usize>();
    let all = entries + lengths(&[0, 7, 8, 9, 6]);
    for (inflated_bytes, fits) in [(all as u64, true), (all as u64 - 1, false)] {
        let pack_copy = dir.path().join(format!("copy-{fits}.pack"));
        std::fs::copy(&pack, &pack_copy).unwrap();
        let limits = IndexLimits {
            inflated_bytes,
            ..IndexLimits::default()
        };
        let indexed = index_pack_thin(&pack_copy, HashAlgo::Sha1, false, &limits, Some(&bases));
        match fits {
            true => assert_eq!(indexed.unwrap().objects, 5),
            false => assert!(matches!(indexed, Err(PackError::Limit("inflated bytes")))),
        }
    }
}

/// A pack nobody waits for any more is scanned no further, and made whole no
/// further: where the peer is gone once the first base is read, the second
/// is not asked for.
#[test]
fn a_pack_nobody_waits_for_is_scanned_and_made_whole_no_further() {
    use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
    let dir = TempDir::new().unwrap();
    let gone = AtomicBool::new(true);
    let limits = IndexLimits {
        given_up: Some(&gone),
        ..IndexLimits::default()
    };
    let plain = written(&dir, &[plain_entry(&object(0))]);
    let scanned = index_pack_within(&plain, HashAlgo::Sha1, false, &limits);
    assert!(matches!(scanned, Err(PackError::GivenUp)), "{scanned:?}");

    // deltas of two objects the pack leaves out
    let (one, two) = (object(7), object(5));
    let entries = [
        ref_entry(&blob(&one), &delta(one.len(), 18, &object(8))),
        ref_entry(&blob(&two), &delta(two.len(), 18, &object(6))),
    ];
    let pack = written(&dir, &entries);
    gone.store(false, Relaxed);
    let (held, asked) = ([one, two], Cell::new(0));
    let found = store(&held, &asked);
    let bases = |oid: &ObjectId| {
        gone.store(true, Relaxed);
        found(oid)
    };
    let made = index_pack_thin(&pack, HashAlgo::Sha1, false, &limits, Some(&bases));
    assert!(matches!(made, Err(PackError::GivenUp)), "{made:?}");
    assert_eq!(asked.get(), 1);
}
