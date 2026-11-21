//! The records an add left unsynced are indexed in boot runs: until a
//! command syncs, the commands after it look records up in those and do
//! not read every record on the way in. The runs hold for the boot they
//! were written in and for as long as no record is taken out of the
//! store.

use std::fs;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};

use super::super::super::sync_order::{FILES, lens, objects_synced};
use super::super::super::unconfirmed::{alt, ok, stderr};
use super::super::base::copy_tree;
use super::{Many, PAGE, RUNS, VERIFIED, files_stored, goes_on, run_ends};

/// The boot run files of the record log `file`.
fn boot_runs(alt_dir: &Path, file: usize) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in fs::read_dir(alt_dir.join(RUNS[file])).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            let runs = fs::read_dir(entry.path()).unwrap();
            found.extend(runs.map(|run| run.unwrap().path()));
        }
    }
    found
}

fn all_boot_runs(alt_dir: &Path) -> Vec<PathBuf> {
    (1..4).flat_map(|file| boot_runs(alt_dir, file)).collect()
}

/// The entries of the run file `run`: a header of 56 bytes, which says
/// how many there are, then 40 bytes for each.
fn entries(run: &[u8]) -> std::ops::Range<usize> {
    let count = u64::from_le_bytes(run[8..16].try_into().unwrap()) as usize;
    56..56 + 40 * count
}

/// Makes every boot run one that reads as a run and holds no entry of
/// the records it covers: whoever looks a record up in it is misled.
fn spoil(alt_dir: &Path) -> Vec<PathBuf> {
    let runs = all_boot_runs(alt_dir);
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

/// A file of each kind the add staged reads from the store through a
/// command that only reads.
fn staged_read(what: &str, dir: &Path) {
    for name in [
        "small/7.txt",
        "small/250.txt",
        "parts/3.zip",
        "chunked/200.bin",
    ] {
        let bytes = fs::read(dir.join(name)).unwrap();
        let oid = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, &bytes);
        let read = alt(dir, &["cat-file", "-p", &oid.to_string()]);
        assert!(read.status.success(), "{what}: {name}: {}", stderr(&read));
        assert!(read.stdout == bytes, "{what}: {name}");
    }
}

#[test]
fn an_add_that_is_not_synced_leaves_boot_runs_no_command_looks_in_after_another_boot() {
    let base = Many::build();
    let dir = base.tmp.path().join("base");
    let alt_dir = dir.join(".alt");
    for file in 1..4 {
        let runs = boot_runs(&alt_dir, file);
        assert_eq!(runs.len(), 1, "{}: {runs:?}", RUNS[file]);
        let ends = run_ends(runs[0].parent().unwrap());
        assert_eq!(ends, [base.ends[file]], "{}", RUNS[file]);
    }
    let status = ok(alt(&dir, &["status"]));
    staged_read("in the boot of the add", &dir);
    // after another boot the files may hold anything
    let what = "after another boot";
    let left = base.left("another-boot", &[]);
    let alt_dir = left.join(".alt");
    let runs = spoil(&alt_dir);
    assert_eq!(runs.len(), 3);
    assert_eq!(ok(alt(&left, &["status"])), status);
    staged_read(what, &left);
    assert!(
        runs.iter().all(|run| spoiled(run)),
        "a reader took them away"
    );
    // the first command that writes takes them away
    fs::write(left.join("one-more.txt"), "one more\n").unwrap();
    ok(alt(&left, &["add", "one-more.txt"]));
    assert!(!all_boot_runs(&alt_dir).iter().any(|run| spoiled(run)));
    staged_read(what, &left);
    goes_on(what, &base, &left, true);
}

#[test]
fn a_page_lost_under_a_boot_run_in_its_boot_is_found_by_the_command_that_syncs() {
    let base = Many::build();
    let dir = base.tmp.path().join("same-boot");
    copy_tree(&base.tmp.path().join("base"), &dir);
    let alt_dir = dir.join(".alt");
    let middle = (base.synced[3] + base.ends[3]) / 2;
    let at = middle - middle % PAGE;
    let mut map = fs::OpenOptions::new();
    let map = map.read(true).write(true);
    let map = map.open(alt_dir.join(FILES[3])).unwrap();
    let mut page = vec![0u8; PAGE as usize];
    map.read_exact_at(&mut page, at).unwrap();
    map.write_all_at(&[0u8; PAGE as usize], at).unwrap();
    // commands that only read, and an add, do not read those records;
    // the check reads every record
    ok(alt(&dir, &["status"]));
    let check = alt(&dir, &["fsck"]);
    assert!(!check.status.success(), "{}", stderr(&check));
    assert!(
        stderr(&check).contains("map.alt record corrupt"),
        "{}",
        stderr(&check)
    );
    fs::write(dir.join("one-more.txt"), "one more\n").unwrap();
    ok(alt(&dir, &["add", "one-more.txt"]));
    let (before, head) = (lens(&alt_dir), ok(alt(&dir, &["rev-parse", "HEAD"])));
    let refused = alt(&dir, &["commit", "-m", "second"]);
    let said = stderr(&refused);
    assert!(!refused.status.success(), "{said}");
    assert!(said.contains("map.alt record corrupt"), "{said}");
    assert_eq!(lens(&alt_dir), before);
    assert_eq!(ok(alt(&dir, &["rev-parse", "HEAD"])), head);
    // with the page as the add wrote it, the commit syncs the records
    // and the boot runs go
    map.write_all_at(&page, at).unwrap();
    ok(alt(&dir, &["commit", "-m", "second"]));
    assert_eq!(objects_synced(&alt_dir), lens(&alt_dir));
    assert_eq!(all_boot_runs(&alt_dir), Vec::<PathBuf>::new());
    staged_read("after the commit", &dir);
    assert!(ok(alt(&dir, &["fsck"])).ends_with(VERIFIED));
    files_stored("after the commit", &dir);
}

#[test]
fn boot_runs_go_with_the_records_a_commit_cuts_off_in_the_boot_they_were_written_in() {
    let base = Many::build();
    let dir = base.tmp.path().join("cut");
    copy_tree(&base.tmp.path().join("base"), &dir);
    let alt_dir = dir.join(".alt");
    // the pack loses what the add appended, in the boot of the add
    let pack = alt_dir.join(FILES[0]);
    let pack = fs::OpenOptions::new().write(true).open(pack).unwrap();
    pack.set_len(base.synced[0]).unwrap();
    assert_eq!(all_boot_runs(&alt_dir).len(), 3);
    let refused = alt(&dir, &["commit", "-m", "second"]);
    let said = stderr(&refused);
    assert!(!refused.status.success(), "{said}");
    assert!(said.contains("add the file again"), "{said}");
    assert_eq!(all_boot_runs(&alt_dir), Vec::<PathBuf>::new());
    assert!(lens(&alt_dir)[3] < base.ends[3], "the records were cut off");
    goes_on("after the cut", &base, &dir, true);
}
