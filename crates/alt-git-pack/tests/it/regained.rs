//! A base that deltas keep coming back to: an object too large to be
//! held beside anything, with many deltas that each have one of their
//! own. It is inflated once and read from a file from then on; where
//! files have no room for it, each time it is inflated again counts
//! against what the pack may inflate to.

use alt_git_codec::HashAlgo;
use alt_git_pack::{IndexLimits, PackError, index_pack_within};
use alt_os::temp::TempDir;

use crate::index_pack::{by_git, delta, ofs_entry, plain_entry, wide, written};

/// The root of a thousand bytes, then `groups` times a delta of the root
/// that makes a few bytes and a delta of that.
fn groups(groups: usize) -> Vec<Vec<u8>> {
    let root = plain_entry(&wide(0));
    let mut entries = vec![root];
    let mut from_root: usize = entries[0].len();
    for n in 0..groups {
        let small = format!("s{n:04}").into_bytes();
        let smaller = format!("t{n:04}").into_bytes();
        let first = ofs_entry(from_root, &delta(1000, 0, &small));
        let second = ofs_entry(first.len(), &delta(small.len(), 0, &smaller));
        from_root += first.len() + second.len();
        entries.extend([first, second]);
    }
    entries
}

#[test]
fn a_base_many_deltas_come_back_to_is_inflated_once_however_many_they_are() {
    for count in [8usize, 64] {
        let dir = TempDir::new().unwrap();
        let entries = groups(count);
        let pack = written(&dir, &entries);
        let deltas = delta(1000, 0, b"s0000").len() + delta(5, 0, b"t0000").len();
        let inflated = 1000 + (count * (2 * 5 + deltas)) as u64;
        // room in memory for half the root: it is let go of with each group
        let tight = IndexLimits {
            base_bytes: 500,
            inflated_bytes: inflated,
            ..IndexLimits::default()
        };
        let indexed = index_pack_within(&pack, HashAlgo::Sha1, false, &tight).unwrap();
        assert_eq!(std::fs::read(&indexed.idx_path).unwrap(), by_git(&pack));
        // every entry once by the scan, every delta once more, the root once
        let once = (1 + 2 * count + 2 * count + 1) as u64;
        assert_eq!(indexed.work.inflated, once, "{count}: {:?}", indexed.work);
        assert_eq!(indexed.work.spilled, 1000, "{count}: {:?}", indexed.work);
        assert!(indexed.work.held <= 1005, "{count}: {:?}", indexed.work);

        // no room in files: the root is inflated again for each group
        // after the first, and that is what the pack is refused for
        let none = IndexLimits {
            spill_bytes: 0,
            ..tight
        };
        let refused = index_pack_within(&pack, HashAlgo::Sha1, false, &none);
        assert!(
            matches!(refused, Err(PackError::Limit("inflated bytes"))),
            "{count}: {refused:?}"
        );
        let again = (count as u64 - 1) * 1000;
        let counted = IndexLimits {
            inflated_bytes: inflated + again,
            ..none
        };
        let indexed = index_pack_within(&pack, HashAlgo::Sha1, false, &counted).unwrap();
        assert_eq!(indexed.work.inflated, once + count as u64 - 1);
        let short = IndexLimits {
            inflated_bytes: inflated + again - 1,
            ..none
        };
        let refused = index_pack_within(&pack, HashAlgo::Sha1, false, &short);
        assert!(matches!(refused, Err(PackError::Limit("inflated bytes"))));
    }
}
