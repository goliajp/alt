//! Lifecycle: reopen visibility, map.alt crash recovery and corruption
//! detection, and a real sha256 repository end to end.

use std::fs::{self, OpenOptions};
use std::io::Write;

use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_odb::{NativeOdb, OdbError};

#[test]
fn reopen_sees_flushed_objects() {
    let dir = tempfile::tempdir().unwrap();
    let data = b"persistent object".to_vec();
    let oid = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, &data);
    {
        let mut odb = NativeOdb::open(dir.path()).unwrap();
        odb.put(oid, ObjectKind::Blob, &data).unwrap();
        odb.flush().unwrap();
    }
    let odb = NativeOdb::open(dir.path()).unwrap();
    assert_eq!(odb.len(), 1);
    assert_eq!(odb.get(&oid).unwrap().unwrap().data, data);
}

#[test]
fn torn_map_tail_is_dropped_and_reput_heals() {
    let dir = tempfile::tempdir().unwrap();
    let data = b"healed object".to_vec();
    let oid = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, &data);
    {
        let mut odb = NativeOdb::open(dir.path()).unwrap();
        odb.put(oid, ObjectKind::Blob, &data).unwrap();
    }

    let map_path = dir.path().join("map.alt");
    let size = fs::metadata(&map_path).unwrap().len();
    OpenOptions::new()
        .write(true)
        .open(&map_path)
        .unwrap()
        .set_len(size - 10)
        .unwrap();

    let mut odb = NativeOdb::open(dir.path()).unwrap();
    assert!(odb.get(&oid).unwrap().is_none(), "torn record is forgotten");
    // content chunks survived; re-put only rewrites the identity record
    odb.put(oid, ObjectKind::Blob, &data).unwrap();
    assert_eq!(odb.get(&oid).unwrap().unwrap().data, data);
}

#[test]
fn corrupt_map_record_mid_file_refuses_to_open() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut odb = NativeOdb::open(dir.path()).unwrap();
        for i in 0..3u8 {
            let data = vec![i; 100];
            let oid = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, &data);
            odb.put(oid, ObjectKind::Blob, &data).unwrap();
        }
    }

    let map_path = dir.path().join("map.alt");
    let mut bytes = fs::read(&map_path).unwrap();
    bytes[5 + 40] ^= 0xFF; // inside the first record: corruption, not a torn tail
    fs::write(&map_path, &bytes).unwrap();

    let err = match NativeOdb::open(dir.path()) {
        Ok(_) => panic!("corrupt map.alt must refuse to open"),
        Err(e) => e,
    };
    assert!(matches!(err, OdbError::Format(_)), "got {err:?}");
}

#[test]
fn torn_map_append_partial_record_is_truncated() {
    let dir = tempfile::tempdir().unwrap();
    let data = b"survivor".to_vec();
    let oid = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, &data);
    {
        let mut odb = NativeOdb::open(dir.path()).unwrap();
        odb.put(oid, ObjectKind::Blob, &data).unwrap();
    }

    let map_path = dir.path().join("map.alt");
    let mut f = OpenOptions::new().append(true).open(&map_path).unwrap();
    f.write_all(&[0xAA; 30]).unwrap(); // crash mid-append
    drop(f);

    let mut odb = NativeOdb::open(dir.path()).unwrap();
    assert_eq!(odb.get(&oid).unwrap().unwrap().data, data);
    // and the store must keep accepting writes after the truncation
    let more = b"after recovery".to_vec();
    let oid2 = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, &more);
    odb.put(oid2, ObjectKind::Blob, &more).unwrap();
    drop(odb);
    let odb = NativeOdb::open(dir.path()).unwrap();
    assert_eq!(odb.get(&oid2).unwrap().unwrap().data, more);
}

#[test]
fn sha256_repository_objects_round_trip() {
    let repo = tempfile::tempdir().unwrap();
    alt_testutil::make_repo(repo.path(), "sha256");

    let dir = tempfile::tempdir().unwrap();
    let mut odb = NativeOdb::open(dir.path()).unwrap();
    let mut oids = Vec::new();
    let count = alt_testutil::for_each_loose(repo.path(), |oid, raw| {
        assert_eq!(oid.algo(), HashAlgo::Sha256);
        odb.put(oid, raw.kind, &raw.data).unwrap();
        oids.push((oid, raw));
    });
    assert!(count > 0);
    odb.flush().unwrap();

    let odb = NativeOdb::open(dir.path()).unwrap();
    for (oid, raw) in &oids {
        let back = odb.get(oid).unwrap().unwrap();
        assert_eq!(back.kind, raw.kind);
        assert_eq!(back.data, raw.data);
        // fidelity read direction: canonical bytes re-hash to the git id
        assert_eq!(
            ObjectId::hash_object(oid.algo(), back.kind, &back.data),
            *oid
        );
    }
}
