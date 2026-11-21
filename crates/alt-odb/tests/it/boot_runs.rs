//! A batch that was not synced leaves more records than an index run is
//! built from: they are indexed in boot runs, which whoever opens the
//! store looks in instead of reading the records, for as long as the
//! boot is the one they were written in and no record was taken out of
//! the store since.

use std::fs;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use alt_git_codec::ObjectKind;
use alt_odb::NativeOdb;

use super::left_unsynced::left_unsynced;
use super::rewind_interrupted::{FILES, Split, cut, lens, object, open, put, reads};

/// The objects of the batch: of one chunk, and stored through the Tier 1
/// map, more of each than a run is built from.
fn batch() -> impl Iterator<Item = u32> {
    (300..1200).filter(|n| n % 3 != 1)
}

/// A store of thirty synced objects and then [`batch`], not synced; the
/// file lengths before the batch.
fn store(dir: &Path) -> [u64; 4] {
    let mut odb = open(dir);
    put(&mut odb, 0..30);
    drop(odb);
    let synced = lens(dir);
    put_unsynced(dir, batch());
    synced
}

/// A handle that only reads, with the prism every third object is
/// stored through.
fn reading(dir: &Path) -> NativeOdb {
    let mut odb = NativeOdb::reading(dir);
    odb.register_prism(Box::new(Split)).unwrap();
    odb
}

fn put_unsynced(dir: &Path, objects: impl Iterator<Item = u32>) {
    let _unsynced = alt_os::durability::leave_unsynced(true);
    let mut odb = open(dir);
    for n in objects {
        let (oid, data) = object(n);
        odb.put(oid, ObjectKind::Blob, &data).unwrap();
    }
    odb.flush().unwrap();
}

/// The boot run files of the record logs.
fn boot_runs(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for log in &FILES[1..] {
        let Ok(listing) = fs::read_dir(dir.join(format!("{log}.idx"))) else {
            continue;
        };
        for entry in listing.map(Result::unwrap) {
            if entry.file_type().unwrap().is_dir() {
                let runs = fs::read_dir(entry.path()).unwrap();
                found.extend(runs.map(|run| run.unwrap().path()));
            }
        }
    }
    found
}

/// Where the index runs of the chain of each record log end.
fn chain_ends(dir: &Path) -> Vec<u64> {
    let mut ends = Vec::new();
    for log in &FILES[1..] {
        let Ok(listing) = fs::read_dir(dir.join(format!("{log}.idx"))) else {
            continue;
        };
        for name in listing.map(|entry| entry.unwrap().file_name().into_string().unwrap()) {
            if let Some((_, end)) = name.strip_suffix(".run").and_then(|n| n.split_once('-')) {
                ends.push(u64::from_str_radix(end, 16).unwrap());
            }
        }
    }
    ends
}

/// The entries of the run file `run`: a header of 56 bytes, which says
/// how many there are, then 40 bytes for each.
fn entries(run: &[u8]) -> std::ops::Range<usize> {
    let count = u64::from_le_bytes(run[8..16].try_into().unwrap()) as usize;
    56..56 + 40 * count
}

/// Makes every boot run one that reads as a run and holds no entry of
/// the records it covers: whoever looks a record up in it is misled.
fn spoil(dir: &Path) -> Vec<PathBuf> {
    let runs = boot_runs(dir);
    for run in &runs {
        let mut bytes = fs::read(run).unwrap();
        let entries = entries(&bytes);
        bytes[entries].fill(0);
        fs::write(run, bytes).unwrap();
    }
    runs
}

fn spoiled(run: &Path) -> bool {
    fs::read(run).is_ok_and(|bytes| bytes[entries(&bytes)].iter().all(|byte| *byte == 0))
}

fn all_read(odb: &NativeOdb) {
    for n in (0..30).chain(batch()) {
        reads(odb, n);
    }
}

#[test]
fn a_batch_left_unsynced_is_looked_up_in_boot_runs_and_read_by_the_writer_that_syncs() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    let synced = store(dir);
    let ends = lens(dir);
    // one for the Tier 1 map and one for map.alt, and no run of a chain
    // past what was synced
    assert_eq!(boot_runs(dir).len(), 2, "{:?}", boot_runs(dir));
    assert!(chain_ends(dir).iter().all(|end| *end <= synced[3]));
    all_read(&reading(dir));
    // a page of map.alt the batch appended is lost in the boot it was
    // written in: nobody reads those records on the way in
    let at = (synced[3] + ends[3]) / 2;
    let at = at - at % 4096;
    let map = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(dir.join(FILES[3]));
    let (map, mut page) = (map.unwrap(), vec![0u8; 4096]);
    map.read_exact_at(&mut page, at).unwrap();
    map.write_all_at(&[0u8; 4096], at).unwrap();
    let reader = reading(dir);
    for n in 0..30 {
        reads(&reader, n);
    }
    let found = batch().filter(|n| matches!(reader.get(&object(*n).0), Ok(Some(_))));
    let found = found.count();
    assert!(found > 500 && found < batch().count(), "{found} found");
    // a writer that leaves its appends unsynced goes on as well
    put_unsynced(dir, 1500..1501);
    // the one that syncs reads every record before it records them as
    // synced, and changes nothing
    let before = lens(dir);
    let mut writer = open(dir);
    let (oid, data) = object(1503);
    let refused = writer.put(oid, ObjectKind::Blob, &data).unwrap_err();
    let refused = format!("{refused:?}");
    assert!(refused.contains("map.alt record corrupt"), "{refused}");
    drop(writer);
    assert_eq!(lens(dir), before);
    assert_eq!(boot_runs(dir).len(), 2);
    // with the page as it was written the batch is synced, indexed by
    // the chain, and the boot runs are gone
    map.write_all_at(&page, at).unwrap();
    let mut writer = open(dir);
    put(&mut writer, 1503..1504);
    drop(writer);
    assert_eq!(boot_runs(dir), Vec::<PathBuf>::new());
    assert!(chain_ends(dir).iter().any(|end| *end > synced[3]));
    let reader = reading(dir);
    all_read(&reader);
    reads(&reader, 1500);
    reads(&reader, 1503);
    open(dir).check().unwrap();
}

#[test]
fn boot_runs_are_not_looked_in_after_another_boot_and_the_next_writer_takes_them_away() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    let synced = store(dir);
    // the same store read through its boot runs, to tell the two apart
    all_read(&reading(dir));
    left_unsynced(dir, synced, false);
    let runs = spoil(dir);
    assert_eq!(runs.len(), 2);
    let reader = reading(dir);
    all_read(&reader);
    drop(reader);
    assert!(
        runs.iter().all(|run| spoiled(run)),
        "a reader changes nothing"
    );
    put_unsynced(dir, 1500..1501);
    assert!(!runs.iter().any(|run| spoiled(run)));
    assert!(!boot_runs(dir).iter().any(|run| spoiled(run)));
    let reader = reading(dir);
    all_read(&reader);
    reads(&reader, 1500);
}

#[test]
fn boot_runs_are_not_looked_in_once_records_were_taken_out_of_the_store() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    store(dir);
    // a batch that is rolled back: its records are cut off, past what
    // the boot runs cover
    {
        let _unsynced = alt_os::durability::leave_unsynced(true);
        let mut odb = open(dir);
        let before = odb.checkpoint().unwrap();
        put(&mut odb, 1500..1510);
        odb.rewind(before).unwrap();
    }
    let runs = spoil(dir);
    assert_eq!(runs.len(), 2);
    let reader = reading(dir);
    all_read(&reader);
    assert!(!reader.contains(&object(1500).0).unwrap());
    drop(reader);
    put_unsynced(dir, 1500..1501);
    assert!(!boot_runs(dir).iter().any(|run| spoiled(run)));
    let reader = reading(dir);
    all_read(&reader);
    reads(&reader, 1500);
}

#[test]
fn a_removal_of_lost_records_takes_the_boot_runs_away() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    store(dir);
    let whole = lens(dir);
    // the content of the last objects of the batch is lost
    put_unsynced(dir, 1500..1560);
    assert_eq!(boot_runs(dir).len(), 2);
    cut(dir, 0, whole[0]);
    let mut odb = open(dir);
    let lost = odb.lost().unwrap();
    assert!(!lost.records.is_empty());
    assert!(odb.drop_lost(&lost).unwrap());
    drop(odb);
    assert_eq!(boot_runs(dir), Vec::<PathBuf>::new());
    let reader = reading(dir);
    all_read(&reader);
    assert!(!reader.contains(&object(1500).0).unwrap());
    drop(reader);
    put_unsynced(dir, 1500..1501);
    reads(&reading(dir), 1500);
    open(dir).check().unwrap();
}

#[test]
fn boot_runs_over_records_that_were_cut_off_are_taken_away_by_the_next_writer() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    let mut odb = open(dir);
    put(&mut odb, 0..30);
    drop(odb);
    // the batch is rolled back once its boot runs are written
    {
        let _unsynced = alt_os::durability::leave_unsynced(true);
        let mut odb = open(dir);
        let before = odb.checkpoint().unwrap();
        for n in batch() {
            let (oid, data) = object(n);
            odb.put(oid, ObjectKind::Blob, &data).unwrap();
        }
        odb.flush().unwrap();
        assert_eq!(boot_runs(dir).len(), 2);
        odb.rewind(before).unwrap();
    }
    assert_eq!(boot_runs(dir).len(), 2, "nobody has written since");
    let reader = reading(dir);
    for n in 0..30 {
        reads(&reader, n);
    }
    assert!(!batch().any(|n| reader.contains(&object(n).0).unwrap()));
    drop(reader);
    // too few records for a run to be written in their place
    put_unsynced(dir, 1500..1501);
    assert_eq!(boot_runs(dir), Vec::<PathBuf>::new());
    let reader = reading(dir);
    reads(&reader, 1500);
    assert!(!batch().any(|n| reader.contains(&object(n).0).unwrap()));
}
