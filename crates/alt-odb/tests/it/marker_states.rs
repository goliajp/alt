//! What the store does about a record that does not read under each state
//! its marker can be in: recording boundaries, recording them with
//! unsynced appends after, written by an older alt, unreadable, removed,
//! and never written. Where the marker tells of the ends of the logs, an
//! unfinished append does not keep the store from opening and is cut off
//! by the next writer, which also gives the store a marker with
//! boundaries; damage is reported and nothing changes. A store that has
//! had boundaries and whose marker is gone or does not read is told
//! nothing: every record that does not read is reported.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::FileExt;
use std::path::Path;

use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_odb::NativeOdb;

use super::marker::{distrust, recorded};

const MAP_REC: u64 = 82;
const OBJECTS: u32 = 40;
const FILES: [&str; 4] = [
    "store/pack-00000001.altpack",
    "store/manifests/blobmap",
    "tier1",
    "map.alt",
];

#[derive(Debug, Clone, Copy, PartialEq)]
enum State {
    Recorded,
    Untrusted,
    Older,
    Unreadable,
    Removed,
    /// a store an older alt wrote: no marker and no head record
    Never,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Fault {
    None,
    /// part of a record at the end of `map.alt`
    Unfinished,
    /// a byte of a synced record of `map.alt`, whole records after it
    Damage,
}

fn object(n: u32) -> (ObjectId, Vec<u8>) {
    let data = format!("object number {n} {}", "x".repeat(60)).into_bytes();
    (
        ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, &data),
        data,
    )
}

fn lens(dir: &Path) -> Vec<u64> {
    let len = |name: &&str| fs::metadata(dir.join(name)).unwrap().len();
    FILES.iter().map(len).collect()
}

fn snapshot(dir: &Path) -> Vec<Option<Vec<u8>>> {
    let read = |name: &&str| fs::read(dir.join(name)).ok();
    FILES.iter().chain(&["odb.durable"]).map(read).collect()
}

fn store(dir: &Path, state: State, fault: Fault) {
    let mut odb = NativeOdb::open(dir).unwrap();
    for n in 0..OBJECTS {
        let (oid, data) = object(n);
        odb.put(oid, ObjectKind::Blob, &data).unwrap();
    }
    odb.flush().unwrap();
    drop(odb);
    let synced = lens(dir);
    let marker = dir.join("odb.durable");
    match state {
        State::Recorded => {}
        State::Untrusted => distrust(dir),
        // the three file lengths an older alt records, and nothing else
        State::Older => {
            let older = [synced[0], synced[1], synced[3]].map(u64::to_le_bytes);
            fs::write(&marker, older.concat()).unwrap();
        }
        State::Unreadable => fs::write(&marker, b"").unwrap(),
        State::Removed => fs::remove_file(&marker).unwrap(),
        State::Never => {
            fs::remove_file(&marker).unwrap();
            fs::write(dir.join("odb.lock"), b"").unwrap();
        }
    }
    let map = OpenOptions::new()
        .write(true)
        .open(dir.join("map.alt"))
        .unwrap();
    match fault {
        Fault::None => {}
        Fault::Unfinished => map.write_all_at(&[0xcd; 30], synced[3]).unwrap(),
        Fault::Damage => map.write_all_at(&[0xff], 5 + 20 * MAP_REC + 40).unwrap(),
    }
}

/// Whether the store opens under `state` with `fault` in it.
fn opens(state: State, fault: Fault) -> bool {
    match fault {
        Fault::None => true,
        // a marker that is gone tells nothing about the end of a log
        Fault::Unfinished => !matches!(state, State::Removed | State::Unreadable),
        Fault::Damage => false,
    }
}

#[test]
fn every_marker_state_tells_an_unfinished_append_from_damage_and_the_next_writer_bounds_the_store()
{
    let states = [
        State::Recorded,
        State::Untrusted,
        State::Older,
        State::Unreadable,
        State::Removed,
        State::Never,
    ];
    for state in states {
        for fault in [Fault::None, Fault::Unfinished, Fault::Damage] {
            let case = format!("{state:?}, {fault:?}");
            let dir = alt_os::temp::tempdir().unwrap();
            store(dir.path(), state, fault);
            let before = snapshot(dir.path());
            let synced_map = 5 + OBJECTS as u64 * MAP_REC;

            let opened = NativeOdb::open(dir.path());
            if !opens(state, fault) {
                assert!(opened.is_err(), "{case}");
                assert!(snapshot(dir.path()) == before, "{case}: nothing changed");
                continue;
            }
            let mut odb = opened.expect(&case);
            if state != State::Never {
                // the opener of a store without a head record writes one,
                // holding the write lock; any other open changes nothing
                assert!(
                    snapshot(dir.path()) == before,
                    "{case}: an open changes nothing"
                );
            }
            for n in 0..OBJECTS {
                let (oid, data) = object(n);
                assert_eq!(odb.get(&oid).unwrap().unwrap().data, data, "{case}");
            }
            let (oid, data) = object(OBJECTS);
            odb.put(oid, ObjectKind::Blob, &data).unwrap();
            odb.flush().unwrap();
            drop(odb);

            let after = lens(dir.path());
            assert_eq!(after[3], synced_map + MAP_REC, "{case}");
            let marker = recorded(dir.path());
            assert_eq!((marker.flags, marker.whole.to_vec()), (0, after), "{case}");
            let odb = NativeOdb::open(dir.path()).unwrap();
            for n in 0..=OBJECTS {
                let (oid, data) = object(n);
                assert_eq!(odb.get(&oid).unwrap().unwrap().data, data, "{case}");
            }
        }
    }
}

#[test]
fn an_older_marker_whose_pack_length_is_of_a_pack_sealed_since_bounds_nothing_in_the_new_one() {
    for (torn, opens) in [(true, true), (false, false)] {
        let dir = alt_os::temp::tempdir().unwrap();
        store(dir.path(), State::Recorded, Fault::None);
        let synced = lens(dir.path());
        let pack = dir.path().join(FILES[0]);
        let older = [synced[0] + 1000, synced[1], synced[3]].map(u64::to_le_bytes);
        fs::write(dir.path().join("odb.durable"), older.concat()).unwrap();
        let file = OpenOptions::new().write(true).open(&pack).unwrap();
        if torn {
            file.write_all_at(&[0xcd; 20], synced[0]).unwrap();
        } else {
            // the encoding byte of the second record
            let first = u32::from_le_bytes(fs::read(&pack).unwrap()[42..46].try_into().unwrap());
            file.write_all_at(&[0xff], 5 + 41 + first as u64 + 32)
                .unwrap();
        }
        let before = snapshot(dir.path());
        let opened = NativeOdb::open(dir.path());
        assert_eq!(opened.is_ok(), opens, "torn {torn}");
        assert!(snapshot(dir.path()) == before);
    }
}

#[test]
fn a_store_without_a_head_record_is_bounded_by_the_open_that_indexes_it() {
    let dir = alt_os::temp::tempdir().unwrap();
    store(dir.path(), State::Never, Fault::Unfinished);
    drop(NativeOdb::open(dir.path()).unwrap());
    let after = lens(dir.path());
    assert_eq!(after[3], 5 + OBJECTS as u64 * MAP_REC, "the open cut it");
    let marker = recorded(dir.path());
    assert_eq!((marker.flags, marker.whole.to_vec()), (0, after));

    // from then on a marker that goes missing was removed
    fs::remove_file(dir.path().join("odb.durable")).unwrap();
    let map = OpenOptions::new()
        .append(true)
        .open(dir.path().join("map.alt"))
        .unwrap();
    (&map).write_all(&[0xcd; 30]).unwrap();
    assert!(NativeOdb::open(dir.path()).is_err());
}

#[test]
fn a_writer_bounds_the_store_before_it_appends() {
    for state in [State::Older, State::Untrusted] {
        let dir = alt_os::temp::tempdir().unwrap();
        store(dir.path(), state, Fault::Unfinished);
        let mut odb = NativeOdb::open(dir.path()).unwrap();
        let (oid, data) = object(OBJECTS);
        odb.put(oid, ObjectKind::Blob, &data).unwrap();
        // the writer dies here: what it found whole is on record
        let marker = recorded(dir.path());
        let mut whole = lens(dir.path());
        whole[0] = marker.whole[0];
        whole[3] -= MAP_REC;
        assert_eq!(
            (marker.flags, marker.whole.to_vec()),
            (0, whole),
            "{state:?}"
        );
        assert!(marker.whole[0] < lens(dir.path())[0]);
        std::mem::forget(odb);
    }
}
