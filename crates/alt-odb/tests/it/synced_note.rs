//! The note in the lock file of how far the active pack is known synced:
//! it follows the synced marker up, a rewind takes it back before it cuts,
//! and an opener whose pack is no longer than it does not check the pack's
//! last records against the marker.

use std::fs::{self, OpenOptions};
use std::os::unix::fs::FileExt;
use std::path::Path;

use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_odb::NativeOdb;

use super::marker::recorded;

const PACK: &str = "store/pack-00000001.altpack";
const NOTE_AT: usize = 1176;

fn object(n: u32) -> (ObjectId, Vec<u8>) {
    let data = format!("object number {n} {}", "x".repeat(60)).into_bytes();
    (
        ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, &data),
        data,
    )
}

fn put(odb: &mut NativeOdb, objects: std::ops::Range<u32>) {
    for n in objects {
        let (oid, data) = object(n);
        odb.put(oid, ObjectKind::Blob, &data).unwrap();
    }
    odb.flush().unwrap();
}

/// The pack and the length the lock file notes as synced.
fn note(dir: &Path) -> Option<(u32, u64)> {
    let lock = fs::read(dir.join("odb.lock")).unwrap();
    let note = lock.get(NOTE_AT..NOTE_AT + 24)?;
    if alt_hash::blake3::hash(&note[..16]).as_bytes()[..8] != note[16..] {
        return None;
    }
    Some((
        u32::from_le_bytes(note[..4].try_into().unwrap()),
        u64::from_le_bytes(note[8..16].try_into().unwrap()),
    ))
}

fn len(dir: &Path) -> u64 {
    fs::metadata(dir.join(PACK)).unwrap().len()
}

#[test]
fn the_note_follows_the_marker_and_a_rewind_brings_it_back() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let mut odb = NativeOdb::open(tmp.path()).unwrap();
    put(&mut odb, 0..10);
    assert_eq!(note(tmp.path()), Some((1, len(tmp.path()))));
    assert_eq!(recorded(tmp.path()).whole[0], len(tmp.path()));
    let ckpt = odb.checkpoint().unwrap();
    let kept = len(tmp.path());
    put(&mut odb, 10..20);
    assert_eq!(note(tmp.path()), Some((1, len(tmp.path()))));
    odb.rewind(ckpt).unwrap();
    assert_eq!(len(tmp.path()), kept);
    assert_eq!(note(tmp.path()), Some((1, kept)));
}

#[test]
fn a_pack_past_the_note_is_checked_against_the_marker_and_one_within_it_is_not() {
    for within in [true, false] {
        let tmp = alt_os::temp::tempdir().unwrap();
        let mut odb = NativeOdb::open(tmp.path()).unwrap();
        put(&mut odb, 0..10);
        let synced = len(tmp.path());
        put(&mut odb, 10..11);
        drop(odb);
        let whole = len(tmp.path());
        // the last record keeps its header and loses its payload; the
        // marker says the pack was synced up to that record
        let pack = OpenOptions::new()
            .write(true)
            .open(tmp.path().join(PACK))
            .unwrap();
        pack.write_all_at(&vec![0u8; (whole - synced - 41) as usize], synced + 41)
            .unwrap();
        super::marker::rewrite(tmp.path(), |marker| {
            marker.eof[0] = synced;
            marker.whole[0] = synced;
        });
        if !within {
            let lock = OpenOptions::new()
                .write(true)
                .open(tmp.path().join("odb.lock"))
                .unwrap();
            lock.write_all_at(&[0u8; 24], NOTE_AT as u64).unwrap();
        }
        let odb = NativeOdb::open(tmp.path()).unwrap();
        let (oid, _) = object(10);
        // within the note the record is taken for synced and reads as
        // damaged; past the note it never finished and is not there
        let err = format!("{:?}", odb.get(&oid).unwrap_err());
        assert_eq!(err.contains("NotFound"), !within, "{err}");
    }
}
