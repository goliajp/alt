//! Readers that opened the store before a compaction and read after it,
//! round after round: every read returns the object, although the packs
//! the reader knew are gone by then.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_odb::NativeOdb;
use alt_store::{BlobOptions, Options};

const PER_ROUND: usize = 300;
const ROUNDS: usize = 12;

fn object(n: usize) -> (ObjectId, Vec<u8>) {
    let data = format!("compacted object {n}\n")
        .repeat(3 + n % 5)
        .into_bytes();
    (
        ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, &data),
        data,
    )
}

#[test]
fn readers_opened_before_each_compaction_read_after_it() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path().to_owned();
    // small packs: each round's objects span several sealed ones, which a
    // reader opening then does not map until it reads from them
    let opts = BlobOptions {
        chunks: Options {
            seal_threshold: 4_000,
            ..Options::default()
        },
        ..BlobOptions::default()
    };
    let mut odb = NativeOdb::open_with(&dir, opts).unwrap();

    // odd: a round's objects are written, readers open; even: the store
    // was compacted, readers read
    let phase = Arc::new(AtomicUsize::new(0));
    let readers: Vec<_> = (0..3)
        .map(|reader| {
            let (dir, phase) = (dir.clone(), Arc::clone(&phase));
            std::thread::spawn(move || {
                let mut reads = 0usize;
                loop {
                    let opened_at = phase.load(Ordering::Acquire);
                    if opened_at >= 2 * ROUNDS {
                        return reads;
                    }
                    if opened_at % 2 == 0 {
                        std::thread::yield_now();
                        continue;
                    }
                    let stored = opened_at.div_ceil(2) * PER_ROUND;
                    let odb = NativeOdb::deferred(&dir);
                    // a lookup opens the store without reading a pack
                    assert!(odb.contains(&object(stored - 1).0).unwrap());
                    while phase.load(Ordering::Acquire) == opened_at {
                        std::thread::yield_now();
                    }
                    for n in (reader..stored).step_by(11) {
                        let (oid, data) = object(n);
                        let got = odb
                            .get(&oid)
                            .unwrap_or_else(|e| panic!("reader {reader}: object {n}: {e:?}"));
                        assert_eq!(got.unwrap().data, data, "object {n}");
                        reads += 1;
                    }
                }
            })
        })
        .collect();

    for round in 0..ROUNDS {
        for n in 0..PER_ROUND {
            let (oid, data) = object(round * PER_ROUND + n);
            odb.put(oid, ObjectKind::Blob, &data).unwrap();
        }
        odb.flush().unwrap();
        phase.store(2 * round + 1, Ordering::Release);
        std::thread::sleep(Duration::from_millis(40));
        odb.compact().unwrap();
        odb.flush().unwrap();
        phase.store(2 * round + 2, Ordering::Release);
        std::thread::sleep(Duration::from_millis(40));
    }
    for reader in readers {
        assert!(reader.join().unwrap() > 0, "every reader read something");
    }
    drop(odb);
    assert_eq!(
        NativeOdb::deferred(&dir).check().unwrap().objects,
        (ROUNDS * PER_ROUND) as u64
    );
}
