//! The note of the boot runs names files that are there: a writer that
//! leaves its appends unsynced looks in no run whose file went, whether
//! a rollback of its own took it or another writer did, and notes what
//! it wrote.

use std::fs;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use alt_git_codec::ObjectKind;
use alt_odb::NativeOdb;

use super::boot_runs::{all_read, boot_runs, put_unsynced, reading, store};
use super::rewind_interrupted::{object, open, put, reads};

/// The index directory of each log the note has a range for.
const INDEXES: [&str; 4] = [
    "store/chunks.idx",
    "store/manifests/blobmap.idx",
    "tier1.idx",
    "map.alt.idx",
];

/// The files the note in the lock file names: a generation, then where
/// the run of each log begins and ends, at byte 1312.
fn noted(dir: &Path) -> Vec<PathBuf> {
    let mut note = [0u8; 72];
    let lock = fs::File::open(dir.join("odb.lock")).unwrap();
    lock.read_exact_at(&mut note, 1312).unwrap();
    let word = |at: usize| u64::from_le_bytes(note[at..at + 8].try_into().unwrap());
    let boot = alt_os::boot::id().expect("this kernel tells its boot");
    let boot: String = boot.iter().map(|byte| format!("{byte:02x}")).collect();
    let runs = (0..4).filter(|log| word(16 + 16 * log) > word(8 + 16 * log));
    let named = runs.map(|log| {
        let (start, end) = (word(8 + 16 * log), word(16 + 16 * log));
        let at = format!("{boot}-{:016x}/{start:012x}-{end:012x}.run", word(0));
        dir.join(INDEXES[log]).join(at)
    });
    named.collect()
}

/// The note names every boot run file there is and no other; how many.
fn all_noted(dir: &Path, what: &str) -> usize {
    let (mut named, mut files) = (noted(dir), boot_runs(dir));
    named.sort();
    files.sort();
    assert_eq!(named, files, "{what}");
    files.len()
}

fn batch(odb: &mut NativeOdb, objects: impl Iterator<Item = u32>) {
    for n in objects {
        let (oid, data) = object(n);
        odb.put(oid, ObjectKind::Blob, &data).unwrap();
    }
    odb.flush().unwrap();
}

#[test]
fn a_batch_after_a_rollback_notes_only_boot_runs_that_are_there() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    store(dir);
    assert_eq!(all_noted(dir, "after the first batch"), 3);
    {
        let _unsynced = alt_os::durability::leave_unsynced(true);
        let mut odb = open(dir);
        let before = odb.checkpoint().unwrap();
        put(&mut odb, 1500..1510);
        odb.rewind(before).unwrap();
        // the store is at another generation: the runs went with the note
        assert_eq!(all_noted(dir, "after the rollback"), 0);
        // the same handle goes on, with objects of one chunk: enough for
        // a run of the chunk index and of map.alt, none for the Tier 1 map
        batch(&mut odb, (3000..3900).step_by(3));
    }
    assert_eq!(all_noted(dir, "after the batch that followed"), 3);
    // the next writer leaves them where they are
    let runs = boot_runs(dir);
    put_unsynced(dir, 5001..5002);
    assert_eq!(boot_runs(dir), runs);
    let reader = reading(dir);
    all_read(&reader);
    for n in (3000..3900).step_by(3).chain([5001]) {
        reads(&reader, n);
    }
    assert!(!reader.contains(&object(1500).0).unwrap());
}

#[test]
fn writers_that_leave_their_appends_unsynced_in_turn_note_only_boot_runs_that_are_there() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    let mut odb = open(dir);
    put(&mut odb, 0..30);
    drop(odb);
    let mut stored: Vec<u32> = (0..30).collect();
    let mut wrote = |odb: &mut NativeOdb, objects: Vec<u32>, what: &str| {
        batch(odb, objects.iter().copied());
        stored.extend(objects);
        assert_eq!(all_noted(dir, what), 3, "{what}");
        let reader = reading(dir);
        stored.iter().for_each(|n| reads(&reader, *n));
    };
    {
        let _unsynced = alt_os::durability::leave_unsynced(true);
        // both read the store before either writes
        let (mut one, mut other) = (open(dir), open(dir));
        let first = (300..1200).filter(|n| n % 3 != 1).collect();
        wrote(&mut one, first, "the first writes");
        // objects of one chunk: none for the Tier 1 map
        let second = (1200..2100).step_by(3).collect();
        wrote(&mut other, second, "the other writes over it");
        // the runs the first holds went, that of the Tier 1 map with
        // nothing appended to it since
        wrote(&mut one, vec![2100, 2103], "the first again");
        wrote(&mut other, vec![2106, 2109], "the other again");
    }
    let mut writer = open(dir);
    put(&mut writer, 5001..5002);
    drop(writer);
    assert_eq!(all_noted(dir, "after a writer that syncs"), 0);
    let reader = reading(dir);
    for n in stored.iter().chain(&[5001]) {
        reads(&reader, *n);
    }
    open(dir).check().unwrap();
}
