//! The synced lengths in `odb.durable` decide what is made of a record
//! that cannot be read: damage in what was synced is reported and no file
//! is touched; what a dead writer left past it is passed over by an open
//! and cut off by the next writer.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::FileExt;
use std::path::Path;

use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_odb::NativeOdb;

const MAP_REC: u64 = 82;
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

fn lens(dir: &Path) -> Vec<u64> {
    let len = |name: &&str| fs::metadata(dir.join(name)).unwrap().len();
    FILES.iter().map(len).collect()
}

fn append(path: &Path, bytes: &[u8]) {
    let mut file = OpenOptions::new().append(true).open(path).unwrap();
    file.write_all(bytes).unwrap();
}

fn flip(path: &Path, at: u64) {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    let mut byte = [0u8];
    file.read_exact_at(&mut byte, at).unwrap();
    file.write_all_at(&[byte[0] ^ 0xff], at).unwrap();
}

/// Where chunk record `n` of a pack starts.
fn pack_record(pack: &[u8], n: usize) -> u64 {
    let mut at = 5usize;
    for _ in 0..n {
        let stored = u32::from_le_bytes(pack[at + 37..at + 41].try_into().unwrap());
        at += 41 + stored as usize;
    }
    at as u64
}

#[test]
fn damage_in_a_synced_pack_whose_index_was_removed_is_reported_and_nothing_is_cut() {
    let dir = alt_os::temp::tempdir().unwrap();
    let mut odb = NativeOdb::open(dir.path()).unwrap();
    put(&mut odb, 0..400);
    odb.flush().unwrap();
    drop(odb);
    let pack = dir.path().join(FILES[0]);
    // the encoding byte of record 100: no record after it can be found
    let at = pack_record(&fs::read(&pack).unwrap(), 100) + 32;
    OpenOptions::new()
        .write(true)
        .open(&pack)
        .unwrap()
        .write_all_at(&[0xff], at)
        .unwrap();
    fs::remove_dir_all(dir.path().join("store/chunks.idx")).unwrap();

    let before = fs::read(&pack).unwrap();
    let opened = NativeOdb::open(dir.path())
        .map(drop)
        .map_err(|e| format!("{e:?}"));
    assert!(
        opened
            .as_ref()
            .is_err_and(|e| e.contains("active pack damaged")),
        "{opened:?}"
    );
    assert!(fs::read(&pack).unwrap() == before, "the pack is as it was");
}

#[test]
fn what_a_dead_writer_left_past_the_synced_lengths_is_cut_off_by_the_next_writer() {
    let dir = alt_os::temp::tempdir().unwrap();
    let mut odb = NativeOdb::open(dir.path()).unwrap();
    put(&mut odb, 0..40);
    odb.flush().unwrap();
    drop(odb);
    let flushed = lens(dir.path());
    for name in FILES {
        // more than one record's worth, none of it readable
        append(&dir.path().join(name), &[0xcd; 300]);
    }
    let with_tails = lens(dir.path());
    let mut odb = NativeOdb::open(dir.path()).unwrap();
    let (oid, data) = object(39);
    assert_eq!(odb.get(&oid).unwrap().unwrap().data, data);
    assert_eq!(lens(dir.path()), with_tails, "an open cuts nothing");
    put(&mut odb, 40..41);
    odb.flush().unwrap();
    let after = lens(dir.path());
    assert!(after[0] > flushed[0] && after[0] < with_tails[0]);
    assert_eq!(after[1..], [flushed[1], flushed[2], flushed[3] + MAP_REC]);
    drop(odb);
    let odb = NativeOdb::open(dir.path()).unwrap();
    for n in 0..41 {
        let (oid, data) = object(n);
        assert_eq!(odb.get(&oid).unwrap().unwrap().data, data);
    }
}

#[test]
fn a_sink_records_how_far_a_deferred_flush_wrote() {
    let dir = alt_os::temp::tempdir().unwrap();
    let mut odb = NativeOdb::open(dir.path()).unwrap();
    odb.set_defer_durability(true);
    put(&mut odb, 0..40);
    odb.flush().unwrap();
    odb.sink().unwrap().fsync().unwrap();
    drop(odb);
    // the last record: only the marker tells it from an unfinished append
    let map = dir.path().join("map.alt");
    let whole = fs::metadata(&map).unwrap().len();
    flip(&map, whole - 20);
    assert!(NativeOdb::open(dir.path()).is_err());
    assert_eq!(fs::metadata(&map).unwrap().len(), whole);
}

fn listing(dir: &Path) -> Vec<std::ffi::OsString> {
    let mut names: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    names.sort();
    names
}

#[test]
fn a_rewind_that_finds_damage_changes_nothing_and_the_store_still_answers() {
    let dir = alt_os::temp::tempdir().unwrap();
    let mut odb = NativeOdb::open(dir.path()).unwrap();
    put(&mut odb, 0..600);
    odb.flush().unwrap();
    // behind the index runs: nothing reads it until the rewind has to
    flip(&dir.path().join("map.alt"), 5 + 100 * MAP_REC + 40);
    let ckpt = odb.checkpoint().unwrap();
    put(&mut odb, 1000..1300);
    odb.flush().unwrap();

    let state = || {
        let marker = fs::read(dir.path().join("odb.durable")).unwrap();
        let runs = ["map.alt.idx", "store/chunks.idx"].map(|idx| listing(&dir.path().join(idx)));
        (lens(dir.path()), marker, runs)
    };
    let before = state();
    assert!(odb.rewind(ckpt).is_err());
    assert!(state() == before, "no file, run or marker changed");
    for n in [599, 1299] {
        let (oid, data) = object(n);
        assert_eq!(odb.get(&oid).unwrap().unwrap().data, data, "object {n}");
    }
}

#[test]
fn a_rewind_brings_the_marker_down_with_the_files() {
    let dir = alt_os::temp::tempdir().unwrap();
    let mut odb = NativeOdb::open(dir.path()).unwrap();
    put(&mut odb, 0..40);
    odb.flush().unwrap();
    let kept = lens(dir.path());
    let ckpt = odb.checkpoint().unwrap();
    put(&mut odb, 40..80);
    odb.flush().unwrap();
    odb.rewind(ckpt).unwrap();
    drop(odb);
    assert_eq!(lens(dir.path()), kept);
    // an unfinished append past the cut is still told from damage
    append(&dir.path().join("map.alt"), &[0xcd; 30]);
    let mut odb = NativeOdb::open(dir.path()).unwrap();
    let (gone, _) = object(79);
    assert!(odb.get(&gone).unwrap().is_none());
    put(&mut odb, 100..101);
    odb.flush().unwrap();
    assert_eq!(lens(dir.path())[3], kept[3] + MAP_REC);
}
