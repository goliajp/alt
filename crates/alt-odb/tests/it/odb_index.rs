//! The odb behind its index runs: every lookup answers the same as it did
//! when the whole store was replayed on open, including for stores written
//! before the runs existed.

use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_odb::{NativeOdb, OdbError};

const OBJECTS: usize = 700;

fn object(n: usize) -> (ObjectId, Vec<u8>) {
    let data = format!("object number {n}\n")
        .repeat(1 + n % 7)
        .into_bytes();
    (
        ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, &data),
        data,
    )
}

fn fill(dir: &Path, range: std::ops::Range<usize>) {
    let mut odb = NativeOdb::open(dir).unwrap();
    for n in range {
        let (oid, data) = object(n);
        odb.put(oid, ObjectKind::Blob, &data).unwrap();
    }
    odb.flush().unwrap();
}

fn index_dirs(dir: &Path) -> Vec<std::path::PathBuf> {
    vec![dir.join("map.alt.idx"), dir.join("store/chunks.idx")]
}

fn check(dir: &Path, count: usize) {
    let odb = NativeOdb::open(dir).unwrap();
    assert_eq!(odb.len().unwrap(), count);
    for n in (0..count).step_by(13).chain([count - 1]) {
        let (oid, data) = object(n);
        assert!(odb.contains(&oid).unwrap());
        let entry = odb.lookup(&oid).unwrap().unwrap();
        assert_eq!(entry.size, data.len() as u64);
        assert_eq!(odb.get(&oid).unwrap().unwrap().data, data);
        assert_eq!(odb.lookup_by_alt(entry.alt).unwrap(), vec![entry]);
        let hex = oid.to_string();
        let found = odb.lookup_by_prefix(&hex[..9], 4).unwrap();
        assert_eq!(found, vec![entry], "prefix {}", &hex[..9]);
    }
    let (absent, _) = object(count);
    assert!(odb.lookup(&absent).unwrap().is_none());
    assert!(odb.get(&absent).unwrap().is_none());
    let order: Vec<ObjectId> = odb.entries().unwrap().iter().map(|e| e.git).collect();
    let expect: Vec<ObjectId> = (0..count).map(|n| object(n).0).collect();
    assert_eq!(order, expect, "entries stay in append order");
}

#[test]
fn indexed_store_answers_like_a_replayed_one() {
    let dir = alt_os::temp::tempdir().unwrap();
    fill(dir.path(), 0..OBJECTS);
    for idx in index_dirs(dir.path()) {
        assert!(idx.is_dir(), "{} is written at flush", idx.display());
    }
    fill(dir.path(), OBJECTS..OBJECTS + 20);
    check(dir.path(), OBJECTS + 20);

    // a store from before the runs existed: no index directories at all
    for idx in index_dirs(dir.path()) {
        fs::remove_dir_all(idx).unwrap();
    }
    check(dir.path(), OBJECTS + 20);
    // the first open indexes it, reader or not
    for idx in index_dirs(dir.path()) {
        assert!(idx.is_dir());
    }
    fill(dir.path(), OBJECTS + 20..OBJECTS + 21);
    check(dir.path(), OBJECTS + 21);
}

#[test]
fn short_prefix_reports_every_match() {
    let dir = alt_os::temp::tempdir().unwrap();
    fill(dir.path(), 0..OBJECTS);
    fill(dir.path(), OBJECTS..OBJECTS + 50);
    let odb = NativeOdb::open(dir.path()).unwrap();
    let all = odb.entries().unwrap();
    for prefix in ["0000", "1a2b", "ffff", "8", "abcde"] {
        let expect = all
            .iter()
            .filter(|e| e.git.to_string().starts_with(prefix))
            .count();
        let found = odb.lookup_by_prefix(prefix, usize::MAX).unwrap();
        let expect = if prefix.len() < 4 { 0 } else { expect };
        assert_eq!(found.len(), expect, "prefix {prefix}");
    }
    // some four-digit prefix is shared by two of the 750 objects
    let mut hexes: Vec<String> = all.iter().map(|e| e.git.to_string()).collect();
    hexes.sort();
    let shared = hexes
        .windows(2)
        .find(|w| w[0][..4] == w[1][..4])
        .map(|w| w[0][..4].to_owned());
    if let Some(prefix) = shared {
        assert!(odb.lookup_by_prefix(&prefix, 4).unwrap().len() >= 2);
    }
}

#[test]
fn rewind_after_an_indexed_batch_leaves_no_trace_of_it() {
    let dir = alt_os::temp::tempdir().unwrap();
    fill(dir.path(), 0..40);
    {
        let mut odb = NativeOdb::open(dir.path()).unwrap();
        let ckpt = odb.checkpoint().unwrap();
        for n in 40..40 + OBJECTS {
            let (oid, data) = object(n);
            odb.put(oid, ObjectKind::Blob, &data).unwrap();
        }
        odb.flush().unwrap();
        assert!(dir.path().join("map.alt.idx").is_dir());
        odb.rewind(ckpt).unwrap();
        assert!(!odb.contains(&object(40).0).unwrap());
        assert!(odb.contains(&object(39).0).unwrap());
    }
    check(dir.path(), 40);
    // different objects land at the offsets the rolled-back batch used
    fill(dir.path(), 2_000..2_000 + OBJECTS);
    let odb = NativeOdb::open(dir.path()).unwrap();
    assert_eq!(odb.len().unwrap(), 40 + OBJECTS);
    assert!(!odb.contains(&object(40).0).unwrap());
    let (oid, data) = object(2_100);
    assert_eq!(odb.get(&oid).unwrap().unwrap().data, data);
}

#[test]
fn a_damaged_map_record_behind_the_index_is_reported_when_read() {
    let dir = alt_os::temp::tempdir().unwrap();
    fill(dir.path(), 0..OBJECTS);
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(dir.path().join("map.alt"))
        .unwrap();
    let at = 5 + 10 * 82 + 70;
    let mut byte = [0u8; 1];
    file.seek(SeekFrom::Start(at)).unwrap();
    file.read_exact(&mut byte).unwrap();
    file.seek(SeekFrom::Start(at)).unwrap();
    file.write_all(&[byte[0] ^ 0x40]).unwrap();
    drop(file);

    let odb = NativeOdb::open(dir.path()).unwrap();
    assert!(odb.lookup(&object(9).0).unwrap().is_some());
    assert!(matches!(
        odb.lookup(&object(10).0),
        Err(OdbError::Format("map.alt record corrupt"))
    ));
    assert!(matches!(
        odb.entries(),
        Err(OdbError::Format("map.alt record corrupt"))
    ));
}

#[test]
fn concurrent_writers_keep_one_consistent_index() {
    let dir = alt_os::temp::tempdir().unwrap();
    let path = dir.path().to_owned();
    drop(NativeOdb::open(&path).unwrap());
    let workers: Vec<_> = (0..4)
        .map(|w| {
            let path = path.clone();
            std::thread::spawn(move || {
                // separate handles, as separate processes would hold
                for round in 0..6 {
                    let base = w * 10_000 + round * 300;
                    fill(&path, base..base + 300);
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    let odb = NativeOdb::open(dir.path()).unwrap();
    assert_eq!(odb.len().unwrap(), 4 * 6 * 300);
    for w in 0..4 {
        for n in (w * 10_000..w * 10_000 + 1_800).step_by(7) {
            let (oid, data) = object(n);
            assert_eq!(odb.get(&oid).unwrap().unwrap().data, data, "object {n}");
        }
    }
}

fn random_bytes(len: usize, seed: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(len + 8);
    let mut state = seed;
    while out.len() < len {
        state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        out.extend_from_slice(&(z ^ (z >> 31)).to_le_bytes());
    }
    out.truncate(len);
    out
}

fn blob(data: &[u8]) -> ObjectId {
    ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, data)
}

/// A small object, one stored as many chunks, and one stored decomposed.
fn three_kinds() -> [Vec<u8>; 3] {
    use alt_prism::Prism;
    let payload = b"a binary asset wrapped in a zlib stream, long enough to pay off".repeat(8);
    let zlib = alt_prism_deflate::DeflatePrism
        .recompose(&[1], &[&payload])
        .unwrap();
    [b"small".to_vec(), random_bytes(3 << 20, 7), zlib]
}

#[test]
fn a_deferred_store_reads_every_kind_of_blob() {
    let dir = alt_os::temp::tempdir().unwrap();
    let kinds = three_kinds();
    {
        let mut odb = NativeOdb::open(dir.path()).unwrap();
        for data in &kinds {
            odb.put(blob(data), ObjectKind::Blob, data).unwrap();
        }
        odb.flush().unwrap();
    }
    assert!(
        fs::metadata(dir.path().join("store/manifests/blobmap"))
            .unwrap()
            .len()
            > 5
    );
    assert!(fs::metadata(dir.path().join("tier1")).unwrap().len() > 5);

    let odb = NativeOdb::deferred(dir.path());
    for data in &kinds {
        assert_eq!(&odb.get(&blob(data)).unwrap().unwrap().data, data);
    }
    assert!(odb.get(&blob(b"absent")).unwrap().is_none());
    // a second handle writing through the deferred path sees them all
    let mut odb = NativeOdb::deferred(dir.path());
    for data in &kinds {
        odb.put(blob(data), ObjectKind::Blob, data).unwrap();
    }
    odb.flush().unwrap();
    assert_eq!(NativeOdb::open(dir.path()).unwrap().len().unwrap(), 3);
}

#[test]
fn a_deferred_store_reports_a_damaged_blob_map_when_it_is_needed() {
    let dir = alt_os::temp::tempdir().unwrap();
    let kinds = three_kinds();
    let more = random_bytes(3 << 20, 8);
    {
        let mut odb = NativeOdb::open(dir.path()).unwrap();
        for data in kinds.iter().chain([&more]) {
            odb.put(blob(data), ObjectKind::Blob, data).unwrap();
        }
        odb.flush().unwrap();
    }
    let path = dir.path().join("store/manifests/blobmap");
    let mut bytes = fs::read(&path).unwrap();
    bytes[5 + 10] ^= 0xff; // inside the first of two records
    fs::write(&path, &bytes).unwrap();

    assert!(
        NativeOdb::open(dir.path()).is_err(),
        "a full open checks it"
    );
    let odb = NativeOdb::deferred(dir.path());
    assert_eq!(odb.get(&blob(&kinds[0])).unwrap().unwrap().data, kinds[0]);
    assert!(odb.get(&blob(&kinds[1])).is_err());
}

#[test]
fn a_map_opened_late_inside_a_write_batch_drops_a_torn_tail_first() {
    let dir = alt_os::temp::tempdir().unwrap();
    let first = random_bytes(3 << 20, 1);
    let second = random_bytes(3 << 20, 2);
    {
        let mut odb = NativeOdb::open(dir.path()).unwrap();
        odb.put(blob(&first), ObjectKind::Blob, &first).unwrap();
        odb.flush().unwrap();
    }
    // a writer died mid-append: half a record after the whole one
    let path = dir.path().join("store/manifests/blobmap");
    OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(&[0xaa; 37])
        .unwrap();
    {
        let mut odb = NativeOdb::deferred(dir.path());
        odb.put(blob(&second), ObjectKind::Blob, &second).unwrap();
        odb.flush().unwrap();
    }
    assert_eq!(fs::metadata(&path).unwrap().len(), 5 + 2 * 80);
    let odb = NativeOdb::open(dir.path()).unwrap();
    assert_eq!(odb.get(&blob(&first)).unwrap().unwrap().data, first);
    assert_eq!(odb.get(&blob(&second)).unwrap().unwrap().data, second);
}
