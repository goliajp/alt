//! Everything that changes `odb.durable` does so one at a time.

use std::fs;
use std::path::Path;

use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_odb::NativeOdb;

use super::marker::recorded;

const FILES: [&str; 4] = [
    "store/pack-00000001.altpack",
    "store/manifests/blobmap",
    "tier1",
    "map.alt",
];

fn object(n: u32) -> (ObjectId, Vec<u8>) {
    let data = format!("object number {n} {}", "x".repeat(60)).into_bytes();
    (
        ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, &data),
        data,
    )
}

fn put(odb: &mut NativeOdb, range: std::ops::Range<u32>) {
    for n in range {
        let (oid, data) = object(n);
        odb.put(oid, ObjectKind::Blob, &data).unwrap();
    }
}

fn lens(dir: &Path) -> [u64; 4] {
    FILES.map(|name| fs::metadata(dir.join(name)).unwrap().len())
}

#[test]
fn a_sink_and_an_inline_flush_record_side_by_side() {
    const ROUNDS: u32 = 2000;
    let dir = alt_os::temp::tempdir().unwrap();
    let path = dir.path().to_owned();
    drop(NativeOdb::open(&path).unwrap());
    let inline = {
        let path = path.clone();
        std::thread::spawn(move || {
            let mut odb = NativeOdb::open(&path).unwrap();
            let mut errors = Vec::new();
            for n in 0..ROUNDS {
                let (oid, data) = object(n);
                let put = odb.put(oid, ObjectKind::Blob, &data);
                if let Err(e) = put.and_then(|_| odb.flush()) {
                    errors.push(format!("flush {n}: {e:?}"));
                }
            }
            errors
        })
    };
    let deferred = {
        let path = path.clone();
        std::thread::spawn(move || {
            let mut odb = NativeOdb::open(&path).unwrap();
            odb.set_defer_durability(true);
            let sink = odb.sink().unwrap();
            let mut errors = Vec::new();
            for n in 100_000..100_000 + ROUNDS {
                let (oid, data) = object(n);
                let put = odb.put(oid, ObjectKind::Blob, &data);
                if let Err(e) = put.and_then(|_| odb.flush()) {
                    errors.push(format!("deferred flush {n}: {e:?}"));
                }
                if let Err(e) = sink.fsync() {
                    errors.push(format!("sink {n}: {e:?}"));
                }
            }
            errors
        })
    };
    let errors = [inline.join().unwrap(), deferred.join().unwrap()].concat();
    assert_eq!(errors, Vec::<String>::new());
    // each recorded how far it wrote and neither undid the other
    assert_eq!(recorded(&path).whole, lens(&path));
}

#[test]
fn a_sink_does_not_record_what_a_rewind_cut_off() {
    let dir = alt_os::temp::tempdir().unwrap();
    let mut odb = NativeOdb::open(dir.path()).unwrap();
    odb.set_defer_durability(true);
    let sink = odb.sink().unwrap();
    put(&mut odb, 0..40);
    odb.flush().unwrap();
    sink.fsync().unwrap();
    let kept = lens(dir.path());
    let before = recorded(dir.path());
    assert_eq!(before.whole, kept);

    let ckpt = odb.checkpoint().unwrap();
    put(&mut odb, 40..80);
    // the sink now has the batch's boundaries to record
    odb.flush().unwrap();
    odb.rewind(ckpt).unwrap();
    assert_eq!(lens(dir.path()), kept);
    sink.fsync().unwrap();
    let after = recorded(dir.path());
    assert_eq!(after.generation, before.generation + 1);
    assert_eq!((after.whole, after.eof), (kept, kept));

    // a batch after the rewind is recorded again
    put(&mut odb, 80..90);
    odb.flush().unwrap();
    sink.fsync().unwrap();
    assert_eq!(recorded(dir.path()).whole, lens(dir.path()));
}
