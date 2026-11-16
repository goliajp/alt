//! Two records for one object in `map.alt`, as two writers storing the same
//! object at once leave them: wherever ids are looked up by prefix, they
//! count as the one object they are.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_odb::NativeOdb;
use alt_store::reclog::INDEX_AT;

/// `map.alt` is a header, then records of one size in append order.
const HEADER: usize = 5;
const RECORD: usize = 82;

fn object(n: usize) -> (ObjectId, Vec<u8>) {
    let data = format!("twice recorded {n}\n").into_bytes();
    let oid = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, &data);
    (oid, data)
}

fn put(dir: &Path, numbers: impl IntoIterator<Item = usize>) {
    let mut odb = NativeOdb::open(dir).unwrap();
    for n in numbers {
        let (oid, data) = object(n);
        odb.put(oid, ObjectKind::Blob, &data).unwrap();
    }
    odb.flush().unwrap();
}

/// Appends the first record of the map once more.
fn record_first_again(dir: &Path) {
    let path = dir.join("map.alt");
    let map = fs::read(&path).unwrap();
    assert_eq!((map.len() - HEADER) % RECORD, 0, "the size of a record");
    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(&map[HEADER..HEADER + RECORD]).unwrap();
}

/// Two objects whose ids agree in their first seven digits or more, the
/// lower id first, and how many digits they agree in.
fn near_pair() -> (usize, usize, usize) {
    let mut ids: Vec<(String, usize)> = (0..80_000).map(|n| (object(n).0.to_string(), n)).collect();
    ids.sort();
    let pair = ids
        .windows(2)
        .find(|w| w[0].0[..7] == w[1].0[..7])
        .expect("80000 ids hold a pair sharing seven digits");
    let shared = pair[0].0.bytes().zip(pair[1].0.bytes());
    let shared = shared.take_while(|(a, b)| a == b).count();
    (pair[0].1, pair[1].1, shared)
}

/// A store where one of a near pair is recorded twice, the second record
/// right after `before` other objects, and its neighbour is stored after
/// that along with `after` more objects.
fn store(before: usize, after: usize) -> (alt_os::temp::TempDir, usize, usize, usize) {
    let (twice, near, shared) = near_pair();
    let dir = alt_os::temp::tempdir().unwrap();
    put(dir.path(), [twice].into_iter().chain(0..before));
    record_first_again(dir.path());
    put(dir.path(), [near].into_iter().chain(before..before + after));
    (dir, twice, near, shared)
}

fn counts_as_one_object(dir: &Path, twice: usize, near: usize, shared: usize) {
    let odb = NativeOdb::deferred(dir);
    let (oid, other) = (object(twice).0, object(near).0);
    let hex = oid.to_string();
    let entry = odb.lookup(&oid).unwrap().unwrap();
    let records = odb.entries().unwrap();
    assert_eq!(records.iter().filter(|e| e.git == oid).count(), 2);

    // under a prefix no other object has, it is the only match
    let found = odb.lookup_by_prefix(&hex[..shared + 1], 2).unwrap();
    assert_eq!(found, vec![entry]);
    // its second record does not take the place of another object
    let found = odb.lookup_by_prefix(&hex[..shared], 2).unwrap();
    let mut found: Vec<ObjectId> = found.iter().map(|e| e.git).collect();
    found.sort();
    assert_eq!(found, [oid, other]);

    let abbrev = odb.abbrev().unwrap();
    assert_eq!(abbrev.of(&oid).unwrap(), hex[..shared + 1]);
    assert_eq!(abbrev.of(&other).unwrap(), other.to_string()[..shared + 1]);
}

#[test]
fn an_object_recorded_twice_in_the_tail_is_one_match() {
    let (dir, twice, near, shared) = store(10, 10);
    counts_as_one_object(dir.path(), twice, near, shared);
}

#[test]
fn an_object_recorded_twice_behind_an_index_run_is_one_match() {
    // the second batch is long enough for its flush to index the whole log
    let (dir, twice, near, shared) = store(10, INDEX_AT + 40);
    assert!(dir.path().join("map.alt.idx").is_dir());
    counts_as_one_object(dir.path(), twice, near, shared);
}
