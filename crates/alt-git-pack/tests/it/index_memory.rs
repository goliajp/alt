//! The memory packs take while they are indexed: three times their
//! largest object and the bases they hold, out of one amount that the
//! packs indexed at once share.

use std::sync::mpsc;
use std::time::Duration;

use alt_git_codec::HashAlgo;
use alt_git_pack::{IndexLimits, Memory, PackError, index_pack_within};
use alt_os::temp::TempDir;

use crate::index_pack::{delta, ofs_entry, plain_entry, wide, written};

/// A pack of an object of a thousand bytes and a delta that makes
/// another of it, to be indexed out of `bytes` of memory.
fn indexed(
    dir: &TempDir,
    bytes: u64,
) -> (
    &'static Memory,
    Result<alt_git_pack::IndexedPackOnDisk, PackError>,
) {
    let base = plain_entry(&wide(0));
    let mut other = wide(0);
    other[999] = b'!';
    let entries = [
        base.clone(),
        ofs_entry(base.len(), &delta(1000, 990, &other)),
    ];
    let memory: &'static Memory = Box::leak(Box::new(Memory::new(bytes)));
    let limits = IndexLimits {
        object_bytes: 1000,
        base_bytes: 0,
        memory: Some(memory),
        ..IndexLimits::default()
    };
    let pack = written(dir, &entries);
    (
        memory,
        index_pack_within(&pack, HashAlgo::Sha1, false, &limits),
    )
}

#[test]
fn a_pack_takes_its_largest_object_three_times_over_and_gives_it_back() {
    let dir = TempDir::new().unwrap();
    let (_, refused) = indexed(&dir, 2999);
    assert!(
        matches!(refused, Err(PackError::Limit("index memory"))),
        "{refused:?}"
    );

    let (memory, taken) = indexed(&dir, 3000);
    let taken = taken.unwrap();
    // while its objects may be read nothing is left of the three thousand
    let (tell, told) = mpsc::channel();
    let waiting = std::thread::spawn(move || {
        let one = memory.take(1).unwrap();
        tell.send(()).unwrap();
        drop(one);
    });
    assert!(told.recv_timeout(Duration::from_millis(300)).is_err());
    drop(taken);
    told.recv().unwrap();
    waiting.join().unwrap();
    drop(memory.take(3000).unwrap());
}
