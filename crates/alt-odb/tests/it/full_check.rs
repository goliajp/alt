//! The full check reads and verifies every record, including the ones no
//! ordinary read has a reason to touch.

use std::fs::OpenOptions;
use std::os::unix::fs::FileExt;
use std::path::Path;

use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_odb::{NativeOdb, OdbError};
use alt_store::StoreError;

const OBJECTS: usize = 700;
/// `map.alt`: a 5-byte header, then 82-byte records.
const MAP_HEADER: u64 = 5;
const MAP_RECORD: u64 = 82;

fn object(n: usize) -> (ObjectId, Vec<u8>) {
    let data = format!("checked object {n}: {}\n", "payload ".repeat(40 + n % 5)).into_bytes();
    (
        ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, &data),
        data,
    )
}

/// A store whose records all sit behind the index runs.
fn fill(dir: &Path) {
    let mut odb = NativeOdb::open(dir).unwrap();
    for n in 0..OBJECTS {
        let (oid, data) = object(n);
        odb.put(oid, ObjectKind::Blob, &data).unwrap();
    }
    odb.flush().unwrap();
    assert!(
        dir.join("map.alt.idx").read_dir().unwrap().next().is_some(),
        "the map has index runs"
    );
}

fn flip(path: &Path, at: u64) {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    let mut byte = [0u8; 1];
    file.read_exact_at(&mut byte, at).unwrap();
    byte[0] ^= 0x04;
    file.write_all_at(&byte, at).unwrap();
}

#[test]
fn a_sound_store_passes_with_every_record_counted() {
    let tmp = alt_os::temp::tempdir().unwrap();
    fill(tmp.path());
    let checked = NativeOdb::deferred(tmp.path()).check().unwrap();
    assert_eq!(checked.objects, OBJECTS as u64);
    assert_eq!(checked.chunk_records, OBJECTS as u64);
    assert_eq!(checked.blob_records, 0);
    assert_eq!(checked.decomposed, 0);
}

#[test]
fn records_superseded_by_a_delta_are_checked_too() {
    let tmp = alt_os::temp::tempdir().unwrap();
    fill(tmp.path());
    // two versions of a file that does not compress on its own
    let mut word = 0x9e37_79b9_7f4a_7c15u64;
    let mut old: Vec<u8> = Vec::new();
    while old.len() < 4096 {
        word ^= word << 13;
        word ^= word >> 7;
        word ^= word << 17;
        old.extend_from_slice(&word.to_le_bytes());
    }
    let mut new = old.clone();
    new[2000..2008].copy_from_slice(b"an edit.");
    let ids =
        [&old, &new].map(|data| ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, data));

    let mut odb = NativeOdb::open(tmp.path()).unwrap();
    odb.put(ids[0], ObjectKind::Blob, &old).unwrap();
    odb.put(ids[1], ObjectKind::Blob, &new).unwrap();
    assert!(odb.lineage_delta(&ids[0], &ids[1]).unwrap());
    odb.flush().unwrap();
    drop(odb);

    // the old version now has two records: the full one and the delta
    let odb = NativeOdb::deferred(tmp.path());
    let checked = odb.check().unwrap();
    assert_eq!(checked.chunk_records, OBJECTS as u64 + 3);
    assert_eq!(checked.objects, OBJECTS as u64 + 2);
    assert_eq!(odb.get(&ids[0]).unwrap().unwrap().data, old);
}

#[test]
fn a_damaged_map_record_no_read_touches_is_found() {
    let tmp = alt_os::temp::tempdir().unwrap();
    fill(tmp.path());
    // the size field of record 350
    flip(
        &tmp.path().join("map.alt"),
        MAP_HEADER + 350 * MAP_RECORD + 70,
    );

    // the store opens and serves every other object as if nothing happened
    let odb = NativeOdb::deferred(tmp.path());
    for n in [0, 349, 351, OBJECTS - 1] {
        let (oid, data) = object(n);
        assert_eq!(odb.get(&oid).unwrap().unwrap().data, data);
    }
    let err = odb.check().unwrap_err();
    assert!(
        matches!(
            err,
            OdbError::Store(StoreError::Format("map.alt record corrupt"))
        ),
        "{err:?}"
    );
}

#[test]
fn a_damaged_chunk_no_read_touches_is_found() {
    let tmp = alt_os::temp::tempdir().unwrap();
    fill(tmp.path());
    let pack = tmp.path().join("store/pack-00000001.altpack");
    // payload of a record in the middle of the pack
    flip(&pack, std::fs::metadata(&pack).unwrap().len() / 2);

    let odb = NativeOdb::deferred(tmp.path());
    for n in [0, OBJECTS - 1] {
        let (oid, data) = object(n);
        assert_eq!(odb.get(&oid).unwrap().unwrap().data, data);
    }
    let err = odb.check().unwrap_err();
    assert!(
        matches!(
            err,
            OdbError::Store(StoreError::Corrupt { .. } | StoreError::Format(_))
        ),
        "{err:?}"
    );
}
