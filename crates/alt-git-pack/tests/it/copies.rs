//! A pack that holds an object many times over, each a delta of the one
//! before by its id.

use alt_git_codec::HashAlgo;
use alt_git_pack::{IndexedPack, index_pack};
use alt_os::temp::TempDir;

use crate::index_pack::{blob, delta, object, plain_entry, ref_entry, written};

/// Every copy is as deep as the first: more of them than a chain may be deep
/// are taken, each its own line of the index.
#[test]
fn copies_of_an_object_each_made_of_the_one_before_lie_no_deeper_than_the_first() {
    let dir = TempDir::new().unwrap();
    let (base, copy) = (object(7), object(8));
    let mut entries = vec![
        plain_entry(&base),
        ref_entry(&blob(&base), &delta(base.len(), 18, &copy)),
    ];
    let again = ref_entry(&blob(&copy), &delta(copy.len(), 18, &copy));
    let copies = alt_git_pack::MAX_DELTA_DEPTH as usize + 905;
    entries.extend(std::iter::repeat_n(again, copies));
    let pack = written(&dir, &entries);
    let indexed = index_pack(&pack, HashAlgo::Sha1, false).unwrap();
    assert_eq!(indexed.objects as usize, copies + 2);
    let read = IndexedPack::open(&pack, HashAlgo::Sha1).unwrap();
    let data = read.read(&blob(&copy)).unwrap().expect("in the pack");
    assert_eq!(*data.data, copy);
}
