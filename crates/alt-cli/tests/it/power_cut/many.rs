//! A power cut after one add of hundreds of files that was not synced:
//! each file of the object store grew by more records than an index run
//! is built from. A page in the middle of what the add appended never
//! reached the disk while what follows it did. No run was built over
//! those records, so whoever opens the store reads them and finds the
//! ones that do not hold.

use std::fs;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use alt_git_codec::{Commit, HashAlgo, ObjectId, ObjectKind, Tree};
use alt_odb::NativeOdb;

use super::super::staged_durability::repo;
use super::super::sync_order::{FILES, lens, objects_synced};
use super::super::unconfirmed::{alt, ok, stderr};
use super::super::unconfirmed_tails::{archive, noise};
use super::base::copy_tree;
use super::{VERIFIED, another_boot, checked, sound, writes};

mod boot_runs;
mod drop_lost;
mod last_page;

/// Records from which a writer that syncs folds a tail into a run.
const RUN_FROM: usize = 256;
const PAGE: u64 = 4096;
/// The directory of index runs of each of [`FILES`].
const RUNS: [&str; 4] = [
    "store/chunks.idx",
    "store/manifests/blobmap.idx",
    "tier1.idx",
    "map.alt.idx",
];

struct Many {
    tmp: alt_os::temp::TempDir,
    head: String,
    synced: [u64; 4],
    ends: [u64; 4],
}

/// Where each index run in `dir` ends in its file.
fn run_ends(dir: &Path) -> Vec<u64> {
    let Ok(listing) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let names = listing.map(|entry| entry.unwrap().file_name().into_string().unwrap());
    let ends = names.filter_map(|name| {
        let (_, end) = name.strip_suffix(".run")?.split_once('-')?;
        // a run of the chunk index names the pack above forty bits
        Some(u64::from_str_radix(end, 16).ok()? & ((1 << 40) - 1))
    });
    ends.collect()
}

impl Many {
    /// One synced commit, then one add that was not synced of small
    /// files, files stored as parts and files of several chunks, more of
    /// each than a run is built from.
    fn build() -> Self {
        let tmp = alt_os::temp::tempdir().unwrap();
        let dir = tmp.path().join("base");
        fs::create_dir(&dir).unwrap();
        repo(&dir);
        let head = ok(alt(&dir, &["rev-parse", "HEAD"])).trim().to_owned();
        let alt_dir = dir.join(".alt");
        let synced = objects_synced(&alt_dir);
        for kind in ["small", "parts", "chunked"] {
            fs::create_dir(dir.join(kind)).unwrap();
        }
        for n in 0..RUN_FROM as u64 + 44 {
            let small = format!("small file {n}\n").repeat(40);
            fs::write(dir.join(format!("small/{n}.txt")), small).unwrap();
        }
        for n in 0..RUN_FROM as u64 + 4 {
            let parts = archive(&noise(n, 3000));
            fs::write(dir.join(format!("parts/{n}.zip")), parts).unwrap();
            // no chunk is longer than a quarter of this
            let chunked = noise(1000 + n, 300 << 10);
            fs::write(dir.join(format!("chunked/{n}.bin")), chunked).unwrap();
        }
        ok(alt(&dir, &["add", "."]));
        let ends = lens(&alt_dir);
        for file in 0..4 {
            let grew = ends[file] - synced[file];
            assert!(grew > 3 * PAGE, "{} grew by {grew}", FILES[file]);
        }
        Self {
            tmp,
            head,
            synced,
            ends,
        }
    }

    /// A copy as a power cut left it: a page in the middle of what the
    /// add appended to each of `files` is zeros, what follows it is
    /// there.
    fn left(&self, name: &str, files: &[usize]) -> PathBuf {
        let dir = self.tmp.path().join(name);
        copy_tree(&self.tmp.path().join("base"), &dir);
        let alt_dir = dir.join(".alt");
        another_boot(&alt_dir);
        for &file in files {
            let middle = (self.synced[file] + self.ends[file]) / 2;
            let at = middle - middle % PAGE;
            assert!(at >= self.synced[file] && at + 2 * PAGE < self.ends[file]);
            let path = alt_dir.join(FILES[file]);
            let stretch = fs::OpenOptions::new().write(true).open(path).unwrap();
            stretch.write_all_at(&[0u8; PAGE as usize], at).unwrap();
        }
        dir
    }
}

/// Every object the commit at HEAD names, down its tree, reads.
fn head_reads(what: &str, dir: &Path) {
    let odb = NativeOdb::read_only(dir.join(".alt"));
    let read = |oid: &ObjectId| {
        let object = odb
            .get(oid)
            .unwrap_or_else(|e| panic!("{what}: {oid}: {e}"));
        object.unwrap_or_else(|| panic!("{what}: {oid} is named and not stored"))
    };
    let head = ok(alt(dir, &["rev-parse", "HEAD"]));
    let head = ObjectId::from_hex(head.trim().as_bytes()).unwrap();
    let commit = Commit::parse(&read(&head).data).unwrap();
    let mut trees = vec![commit.tree().unwrap()];
    while let Some(tree) = trees.pop() {
        for entry in Tree::parse(&read(&tree).data, HashAlgo::Sha1)
            .unwrap()
            .entries
        {
            match entry.mode.is_tree() {
                true => trees.push(entry.oid),
                false => drop(read(&entry.oid)),
            }
        }
    }
}

/// Every file of the work tree is stored as it is on disk.
fn files_stored(what: &str, dir: &Path) {
    let odb = NativeOdb::read_only(dir.join(".alt"));
    for kind in ["small", "parts", "chunked"] {
        for entry in fs::read_dir(dir.join(kind)).unwrap() {
            let path = entry.unwrap().path();
            let bytes = fs::read(&path).unwrap();
            let oid = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, &bytes);
            let stored = odb.get(&oid).unwrap();
            let stored = stored.unwrap_or_else(|| panic!("{what}: {path:?} is not stored"));
            assert!(stored.data == bytes, "{what}: {path:?}");
        }
    }
}

/// Every command goes on in `dir`; no commit names an object that does
/// not read; what the add staged is committed once it is added again.
fn goes_on(what: &str, base: &Many, dir: &Path, adds_first: bool) {
    let status = alt(dir, &["status"]);
    sound(what, &status);
    assert!(status.status.success(), "{what}: {}", stderr(&status));
    checked(what, &alt(dir, &["fsck"]));
    assert_eq!(ok(alt(dir, &["rev-parse", "HEAD"])).trim(), base.head);
    if !adds_first {
        // a commit of what is staged takes nothing that is not stored
        writes(what, dir, alt, &["commit", "-m", "what was left"]);
        checked(what, &alt(dir, &["fsck"]));
        head_reads(what, dir);
    }
    let added = writes(what, dir, alt, &["add", "."]);
    assert!(added.status.success(), "{what}: add: {}", stderr(&added));
    if !ok(alt(dir, &["status"])).contains("nothing to commit") {
        let committed = writes(what, dir, alt, &["commit", "-m", "second"]);
        assert!(committed.status.success(), "{what}: {}", stderr(&committed));
    }
    assert!(ok(alt(dir, &["status"])).contains("nothing to commit"));
    let check = alt(dir, &["fsck"]);
    assert!(stderr(&check).is_empty(), "{what}: {}", stderr(&check));
    assert!(ok(check).ends_with(VERIFIED), "{what}");
    let history = ok(alt(dir, &["log"]));
    let first = history.lines().last().unwrap_or_default();
    assert!(first.starts_with(&base.head), "{what}: {history}");
    head_reads(what, dir);
    files_stored(what, dir);
}

#[test]
fn an_add_that_is_not_synced_builds_no_index_run_over_what_it_appended() {
    let base = Many::build();
    let dir = base.tmp.path().join("base");
    let alt_dir = dir.join(".alt");
    let past = |file: usize, synced: u64| {
        let ends = run_ends(&alt_dir.join(RUNS[file])).into_iter();
        ends.filter(|end| *end > synced).collect::<Vec<u64>>()
    };
    for file in 0..4 {
        let past = past(file, base.synced[file]);
        assert!(past.is_empty(), "{}: runs end at {past:?}", RUNS[file]);
    }
    // the commit syncs the records and builds the runs over them
    ok(alt(&dir, &["commit", "-m", "second"]));
    let synced = objects_synced(&alt_dir);
    assert_eq!(synced, lens(&alt_dir));
    for file in 0..4 {
        let ends = run_ends(&alt_dir.join(RUNS[file]));
        let reach = ends.iter().max().copied().unwrap_or(0);
        assert!(reach > base.synced[file], "{}: {ends:?}", RUNS[file]);
        assert!(past(file, synced[file]).is_empty(), "{}", RUNS[file]);
    }
}

fn a_page_lost_in(sets: &[&[usize]]) {
    let base = Many::build();
    for (case, files) in sets.iter().enumerate() {
        let names: Vec<&str> = files.iter().map(|file| FILES[*file]).collect();
        for adds_first in [false, true] {
            let what = format!("a page of {names:?} lost, adds first {adds_first}");
            let dir = base.left(&format!("{case}-{adds_first}"), files);
            goes_on(&what, &base, &dir, adds_first);
            fs::remove_dir_all(dir).unwrap();
        }
    }
}

#[test]
fn a_page_of_the_pack_lost_within_an_add_of_hundreds_of_files_leaves_every_command_going() {
    a_page_lost_in(&[&[0]]);
}

#[test]
fn a_page_of_the_blob_map_lost_within_an_add_of_hundreds_of_files_leaves_every_command_going() {
    a_page_lost_in(&[&[1]]);
}

#[test]
fn a_page_of_the_tier1_map_lost_within_an_add_of_hundreds_of_files_leaves_every_command_going() {
    a_page_lost_in(&[&[2]]);
}

#[test]
fn a_page_of_the_map_lost_within_an_add_of_hundreds_of_files_leaves_every_command_going() {
    a_page_lost_in(&[&[3]]);
}

#[test]
fn pages_of_several_files_lost_within_an_add_of_hundreds_of_files_leave_every_command_going() {
    a_page_lost_in(&[&[0, 3], &[1, 2], &[0, 1], &[2, 3], &[0, 1, 2, 3]]);
}
