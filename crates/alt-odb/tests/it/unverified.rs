//! An object read without hashing what is read: the read of a caller
//! that had every record of the store verified before.

use std::fs;
use std::os::unix::fs::FileExt;

use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_odb::NativeOdb;

#[test]
fn an_object_read_unverified_is_what_the_pack_holds_whether_or_not_it_hashes_to_its_id() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    // too short to be compressed: the pack holds the bytes as they are
    let data = b"stored as it is\n";
    let oid = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Tree, data);
    let other = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Tree, b"never stored");
    let mut odb = NativeOdb::open(dir).unwrap();
    odb.put(oid, ObjectKind::Tree, data).unwrap();
    odb.flush().unwrap();
    let read = odb.get_unverified(&oid).unwrap().unwrap();
    assert_eq!((read.kind, &read.data[..]), (ObjectKind::Tree, &data[..]));
    assert!(odb.get_unverified(&other).unwrap().is_none());
    odb.check().unwrap();
    drop(odb);

    let pack = dir.join("store/pack-00000001.altpack");
    let bytes = fs::read(&pack).unwrap();
    let at = bytes.windows(data.len()).position(|w| w == data).unwrap();
    let file = fs::OpenOptions::new().write(true).open(&pack).unwrap();
    file.write_all_at(b"S", at as u64).unwrap();
    let odb = NativeOdb::open(dir).unwrap();
    // the full check and the read that hashes tell; this read does not
    assert!(odb.check().is_err());
    assert!(odb.get(&oid).is_err());
    let read = odb.get_unverified(&oid).unwrap().unwrap();
    assert_eq!(read.data, b"Stored as it is\n");
}
