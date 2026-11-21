//! Readers opening the store while a writer keeps folding new records into
//! the index runs, merging and deleting older run files as it goes: every
//! object already flushed is found by every reader, whichever run files it
//! caught on disk.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_odb::NativeOdb;

const BATCH: usize = 300;
const BATCHES: usize = 40;

fn object(n: usize) -> (ObjectId, Vec<u8>) {
    let data = format!("object {n} behind merging runs\n").into_bytes();
    (
        ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, &data),
        data,
    )
}

#[test]
fn readers_find_every_flushed_object_while_runs_are_merged() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path().to_owned();
    // how many objects are flushed, and so must be found by any open
    let flushed = Arc::new(AtomicUsize::new(0));
    drop(NativeOdb::open(&dir).unwrap());

    let readers: Vec<_> = (0..4)
        .map(|reader| {
            let (dir, flushed) = (dir.clone(), Arc::clone(&flushed));
            std::thread::spawn(move || {
                let mut opens = 0usize;
                loop {
                    let visible = flushed.load(Ordering::Acquire);
                    let odb = NativeOdb::deferred(&dir);
                    for n in (reader..visible).step_by(37).chain(visible.checked_sub(1)) {
                        let (oid, data) = object(n);
                        let got = odb
                            .get(&oid)
                            .unwrap_or_else(|e| panic!("reader {reader}: object {n}: {e:?}"))
                            .unwrap_or_else(|| panic!("reader {reader}: object {n} not found"));
                        assert_eq!(got.data, data);
                    }
                    opens += 1;
                    if visible == BATCH * BATCHES {
                        return opens;
                    }
                }
            })
        })
        .collect();

    // every batch is long enough to become a run, and runs of this size
    // are merged into their neighbours again and again
    for batch in 0..BATCHES {
        let mut odb = NativeOdb::deferred(&dir);
        for n in batch * BATCH..(batch + 1) * BATCH {
            let (oid, data) = object(n);
            odb.put(oid, ObjectKind::Blob, &data).unwrap();
        }
        odb.flush().unwrap();
        flushed.store((batch + 1) * BATCH, Ordering::Release);
    }
    for reader in readers {
        assert!(
            reader.join().unwrap() > 1,
            "every reader opened more than once"
        );
    }
    let runs = std::fs::read_dir(dir.join("map.alt.idx")).unwrap().count();
    assert!(runs < 8, "the runs were merged: {runs} left");
    assert_eq!(
        NativeOdb::deferred(&dir).check().unwrap().objects,
        (BATCH * BATCHES) as u64
    );
}
