//! A record that does not read under a boot run, in the boot it was
//! left unsynced in, found by a writer that leaves its own appends
//! unsynced only once it lets go of the run: another writer wrote the
//! runs anew, so the ones this handle holds are no longer the ones
//! noted, and the records they covered are read. The log ends at that
//! record for it as for any writer: it cuts there and goes on.

use std::fs;
use std::os::unix::fs::FileExt;

use alt_git_codec::ObjectKind;
use alt_odb::NativeOdb;

use super::boot_runs::reading;
use super::rewind_interrupted::{FILES, lens, object, open, put, reads};

fn batch(odb: &mut NativeOdb, objects: impl Iterator<Item = u32>) {
    for n in objects {
        let (oid, data) = object(n);
        odb.put(oid, ObjectKind::Blob, &data).unwrap();
    }
    odb.flush().unwrap();
}

#[test]
fn a_writer_that_syncs_nothing_cuts_at_a_record_it_reads_once_it_lets_go_of_its_boot_run() {
    // the blob map and the Tier 1 map
    for file in [1, 2] {
        let tmp = alt_os::temp::tempdir().unwrap();
        let dir = tmp.path();
        let mut odb = open(dir);
        put(&mut odb, 0..30);
        drop(odb);
        let synced = lens(dir);
        let _unsynced = alt_os::durability::leave_unsynced(true);
        // both read the store before either writes
        let (mut one, mut other) = (open(dir), open(dir));
        batch(&mut one, 300..1200);
        let ends = lens(dir);
        batch(&mut other, (1200..2100).filter(|n| n % 3 != 1));
        // a page among what the first appended, which it looks up in
        // its run and does not read
        let at = (synced[file] + ends[file]) / 2;
        let log = fs::OpenOptions::new()
            .write(true)
            .open(dir.join(FILES[file]));
        log.unwrap().write_all_at(&[0; 4096], at).unwrap();

        batch(&mut one, [2100, 2103].into_iter());
        let cut = one.take_discarded();
        let cut_there = |cut: &alt_odb::Discarded| {
            cut.file.ends_with(FILES[file]) && cut.from > synced[file] && cut.from <= at
        };
        assert!(cut.iter().any(cut_there), "{}: {cut:?}", FILES[file]);
        drop((one, other));
        let reader = reading(dir);
        for n in (0..30).chain([2100, 2103]) {
            reads(&reader, n);
        }
        let stored = (300..2100).filter(|n| reader.contains(&object(*n).0).unwrap());
        let stored: Vec<u32> = stored.collect();
        assert!(!stored.is_empty() && stored.len() < 1500, "{}", FILES[file]);
        stored.iter().for_each(|n| reads(&reader, *n));
        open(dir).check().unwrap();
    }
}
