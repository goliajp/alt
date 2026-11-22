//! The records an add left unsynced are indexed in boot runs: until a
//! command syncs, the commands after it look records up in those and do
//! not read every record on the way in. The runs hold for the boot they
//! were written in and for as long as no record is taken out of the
//! store.

use std::fs;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};

use super::super::super::sync_order::{FILES, lens};
use super::super::super::unconfirmed::{alt, ok, stderr};
use super::super::base::copy_tree;
use super::{Many, PAGE, RUNS, goes_on, run_ends};

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
    // the blob map, the Tier 1 map and map.alt
    for file in 1..4 {
        let what = format!("a page of {} lost in the boot of the add", FILES[file]);
        let dir = base.tmp.path().join(format!("same-boot-{file}"));
        copy_tree(&base.tmp.path().join("base"), &dir);
        let alt_dir = dir.join(".alt");
        let middle = (base.synced[file] + base.ends[file]) / 2;
        let at = middle - middle % PAGE;
        let log = fs::OpenOptions::new()
            .write(true)
            .open(alt_dir.join(FILES[file]));
        log.unwrap()
            .write_all_at(&[0u8; PAGE as usize], at)
            .unwrap();
        // commands that only read, and an add, do not read those
        // records; the check reads every record
        ok(alt(&dir, &["status"]));
        let check = alt(&dir, &["fsck"]);
        assert!(!check.status.success(), "{what}: {}", stderr(&check));
        assert!(stderr(&check).contains("record corrupt"), "{what}");
        fs::write(dir.join("one-more.txt"), "one more\n").unwrap();
        ok(alt(&dir, &["add", "one-more.txt"]));
        // the one that syncs reads them before it records them as
        // synced: the log ends for it at the first that does not read,
        // as after a power cut, and what the add staged with a blob
        // that went is to be added again
        let (ended, head) = (lens(&alt_dir)[file], ok(alt(&dir, &["rev-parse", "HEAD"])));
        let refused = alt(&dir, &["commit", "-m", "second"]);
        let said = stderr(&refused);
        assert!(!refused.status.success(), "{what}: {said}");
        assert!(said.contains("add the file again"), "{what}: {said}");
        // from the record the page begins in on
        let discarded = format!(" byte(s) at the end of {}", FILES[file]);
        let cut = said.split("discarded ").find_map(|rest| {
            let bytes = rest.split_once(&discarded)?.0.parse::<u64>().ok()?;
            Some(ended - bytes)
        });
        assert!(
            cut.is_some_and(|cut| cut > base.synced[file] && cut <= at),
            "{what}: {said}"
        );
        assert_eq!(ok(alt(&dir, &["rev-parse", "HEAD"])), head);
        assert_eq!(boot_runs(&alt_dir, 0), Vec::<PathBuf>::new(), "{what}");
        assert_eq!(all_boot_runs(&alt_dir), Vec::<PathBuf>::new(), "{what}");
        goes_on(&what, &base, &dir, true);
        fs::remove_dir_all(dir).unwrap();
    }
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

/// Makes the boot run of the chunk index one that holds no entry.
fn spoil_chunk_run(alt_dir: &Path) -> PathBuf {
    let run = boot_runs(alt_dir, 0).pop().unwrap();
    let mut bytes = fs::read(&run).unwrap();
    let entries = entries(&bytes);
    bytes[entries].fill(0);
    fs::write(&run, bytes).unwrap();
    run
}

#[test]
fn the_boot_run_of_the_chunk_index_is_not_looked_in_after_another_boot() {
    let base = Many::build();
    let dir = base.tmp.path().join("base");
    let alt_dir = dir.join(".alt");
    // one run, up to the end of the pack
    let runs = boot_runs(&alt_dir, 0);
    assert_eq!(runs.len(), 1, "{runs:?}");
    let ends = run_ends(runs[0].parent().unwrap());
    assert_eq!(ends, [base.ends[0]]);
    let status = ok(alt(&dir, &["status"]));
    staged_read("in the boot of the add", &dir);
    let what = "after another boot";
    let left = base.left("another-boot-chunks", &[]);
    let alt_dir = left.join(".alt");
    let run = spoil_chunk_run(&alt_dir);
    assert_eq!(ok(alt(&left, &["status"])), status);
    staged_read(what, &left);
    assert!(spoiled(&run), "a reader took it away");
    fs::write(left.join("one-more.txt"), "one more\n").unwrap();
    ok(alt(&left, &["add", "one-more.txt"]));
    assert!(!spoiled(&run));
    staged_read(what, &left);
    goes_on(what, &base, &left, true);
}

#[test]
fn a_page_of_the_pack_lost_under_its_boot_run_in_its_boot_is_found_by_the_command_that_syncs() {
    let base = Many::build();
    let dir = base.tmp.path().join("same-boot-pack");
    copy_tree(&base.tmp.path().join("base"), &dir);
    let alt_dir = dir.join(".alt");
    let middle = (base.synced[0] + base.ends[0]) / 2;
    let at = middle - middle % PAGE;
    let pack = fs::OpenOptions::new()
        .write(true)
        .open(alt_dir.join(FILES[0]));
    pack.unwrap()
        .write_all_at(&[0u8; PAGE as usize], at)
        .unwrap();
    // a command that only reads does not walk the pack
    ok(alt(&dir, &["status"]));
    // the one that syncs walks it and hashes what it is about to record
    // as synced: the pack ends for it at the first record that does not
    // hold what it names, as after a power cut
    let refused = alt(&dir, &["commit", "-m", "second"]);
    let said = stderr(&refused);
    assert!(!refused.status.success(), "{said}");
    assert!(said.contains("add the file again"), "{said}");
    // from the record the page lies in on
    let cut = said.split("discarded ").nth(1).and_then(|rest| {
        let (bytes, of) = rest.split_once(" byte(s) at the end of ")?;
        let bytes = bytes.parse::<u64>().ok()?;
        of.starts_with(FILES[0]).then(|| base.ends[0] - bytes)
    });
    assert!(
        cut.is_some_and(|cut| cut > base.synced[0] && cut <= at),
        "{said}"
    );
    assert_eq!(boot_runs(&alt_dir, 0), Vec::<PathBuf>::new());
    assert_eq!(all_boot_runs(&alt_dir), Vec::<PathBuf>::new());
    goes_on("after the page", &base, &dir, true);
}
