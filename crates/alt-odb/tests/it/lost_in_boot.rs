//! A record of the blob map, the Tier 1 map or `map.alt` that does not
//! read, in the boot it was left unsynced in, with records after it that
//! do or as the last of the log: a page of the file the kernel lost. To a writer that syncs the
//! log ends there, as what another boot left does: it cuts the log at
//! that record, with what names the content that went, and goes on. A
//! record that was synced and does not read is damage: nothing is cut.

use std::fs;
use std::os::unix::fs::FileExt;
use std::path::Path;

use super::boot_runs::{boot_runs, put_unsynced, reading};
use super::dangling::{snapshot, try_put};
use super::rewind_interrupted::{FILES, KEPT, lens, object, open, put, reads};

fn copy(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap().map(Result::unwrap) {
        let target = to.join(entry.file_name());
        match entry.file_type().unwrap().is_dir() {
            true => copy(&entry.path(), &target),
            false => drop(fs::copy(entry.path(), target).unwrap()),
        }
    }
}

fn zero(dir: &Path, file: usize, at: u64, len: u64) {
    let log = fs::OpenOptions::new()
        .write(true)
        .open(dir.join(FILES[file]));
    log.unwrap()
        .write_all_at(&vec![0; len as usize], at)
        .unwrap();
}

/// What is left of a copy of `base` whose log `file` lost the stretch
/// at `at`, after a writer that syncs stored one more object.
fn cut_at(base: &Path, batch: &[u32], file: usize, (synced, at, len): (u64, u64, u64), case: &str) {
    let dir = base.with_file_name(format!("cut-{file}"));
    copy(base, &dir);
    zero(&dir, file, at, len);
    let generation = open(&dir).generation();
    let mut writer = try_put(&dir, 5001).unwrap_or_else(|e| panic!("{case}: {e:?}"));
    let cut = writer.take_discarded();
    let cut_there = |cut: &alt_odb::Discarded| {
        cut.file.ends_with(FILES[file]) && cut.from > synced && cut.from <= at
    };
    assert!(cut.iter().any(cut_there), "{case}: {cut:?}");
    drop(writer);
    // whole records went: the store is at another generation
    assert_ne!(open(&dir).generation(), generation, "{case}");
    assert_eq!(boot_runs(&dir), Vec::<std::path::PathBuf>::new(), "{case}");
    let reader = reading(&dir);
    for n in (0..KEPT).chain([5001]) {
        reads(&reader, n);
    }
    let stored = |n: &&u32| reader.contains(&object(**n).0).unwrap();
    let kept: Vec<u32> = batch.iter().filter(stored).copied().collect();
    assert!(!kept.is_empty() && kept.len() < batch.len(), "{case}");
    for n in kept {
        reads(&reader, n);
    }
    drop(reader);
    // what went is taken again
    let (first, last) = (batch[0], batch[batch.len() - 1]);
    let mut odb = open(&dir);
    put(&mut odb, first..(last + 1).min(first + 40));
    drop(odb);
    let odb = open(&dir);
    for n in first..(last + 1).min(first + 40) {
        reads(&odb, n);
    }
    odb.check().unwrap_or_else(|e| panic!("{case}: {e:?}"));
    fs::remove_dir_all(dir).unwrap();
}

/// A copy of `base` whose log `file` lost the stretch at `at`, among
/// records that were synced: a writer that syncs reports it.
fn reported(base: &Path, file: usize, at: u64, case: &str) {
    let dir = base.with_file_name(format!("synced-{file}"));
    copy(base, &dir);
    zero(&dir, file, at, 40);
    let before = snapshot(&dir);
    // by the open that reads the record, or by the first write
    let stored = alt_odb::NativeOdb::open(&dir).and_then(|mut odb| {
        let (oid, data) = object(5001);
        odb.put(oid, alt_git_codec::ObjectKind::Blob, &data)?;
        odb.flush()
    });
    let refused = stored.err().map(|e| format!("{e:?}"));
    assert!(
        refused.as_ref().is_some_and(|e| e.contains("corrupt")),
        "{case}: {refused:?}"
    );
    assert_eq!(snapshot(&dir), before, "{case}");
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_writer_that_syncs_cuts_a_log_the_running_boot_left_unsynced_at_a_record_that_does_not_read() {
    // few records, which every open reads; and more than a boot run is
    // built from, which nobody reads on the way in
    for batch in [300..336, 300..1200] {
        let batch: Vec<u32> = batch.collect();
        let tmp = alt_os::temp::tempdir().unwrap();
        let base = tmp.path().join("base");
        let mut odb = open(&base);
        put(&mut odb, 0..KEPT);
        drop(odb);
        let synced = lens(&base);
        put_unsynced(&base, batch.iter().copied());
        let ends = lens(&base);
        let runs = boot_runs(&base).len();
        assert_eq!(runs, if batch.len() > 40 { 4 } else { 0 });
        for file in 1..4 {
            let case = format!("{} with {runs} boot runs", FILES[file]);
            // in the middle of what the batch appended, with records
            // after it
            let at = (synced[file] + ends[file]) / 2;
            let len = ((ends[file] - at) / 2).min(4096);
            cut_at(&base, &batch, file, (synced[file], at, len), &case);
            // the last of what the batch appended, with nothing after it
            let len = ((ends[file] - synced[file]) / 2).min(4096);
            let last = (synced[file], ends[file] - len, len);
            cut_at(&base, &batch, file, last, &format!("{case}, at its end"));
            reported(&base, file, synced[file] / 2, &case);
        }
    }
}

/// The same of the pack, where the stretch that was lost holds the head
/// of a record: the walk of the pack stops there and cannot go on to the
/// records after it.
#[test]
fn a_writer_that_syncs_cuts_the_pack_the_running_boot_left_unsynced_where_the_head_of_a_record_went()
 {
    for batch in [300..336, 300..1200] {
        let batch: Vec<u32> = batch.collect();
        let tmp = alt_os::temp::tempdir().unwrap();
        let base = tmp.path().join("base");
        let mut odb = open(&base);
        put(&mut odb, 0..KEPT);
        drop(odb);
        let synced = lens(&base);
        put_unsynced(&base, batch.iter().copied());
        let ends = lens(&base);
        let runs = boot_runs(&base).len();
        let case = format!("{} with {runs} boot runs", FILES[0]);
        // longer than any chunk is, in the middle of what the batch
        // appended
        let at = (synced[0] + ends[0]) / 2;
        let len = ((ends[0] - at) / 2).min(400_000);
        assert!(len > 300_000, "{case}: {len}");
        cut_at(&base, &batch, 0, (synced[0], at, len), &case);
    }
}
