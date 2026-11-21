//! An older build, which keeps no note of the boot unsynced appends were
//! made in, used first after a power cut.

use std::fs;

use std::path::Path;

use super::super::mixed_versions::{head_tree, new_bin, old_bin, run, same_reads};
use super::super::staged_durability::{add, repo};
use super::super::sync_order::{FILES, lens, objects_synced};
use super::another_boot;

mod head_lost;
#[cfg(target_os = "linux")]
mod synced_first;

fn ok(out: std::process::Output) -> String {
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "{said}");
    String::from_utf8(out.stdout).unwrap()
}

#[test]
#[ignore = "run with ALT_OLD_BIN set to an alt built from the base branch"]
fn an_older_build_first_after_a_power_cut_goes_on_where_the_files_are_whole() {
    let (old, new) = (old_bin(), new_bin());
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    repo(dir);
    add(dir, "b.txt");
    another_boot(&dir.join(".alt"));
    fs::write(dir.join("c.txt"), "c\n").unwrap();
    ok(run(&old, dir, &["add", "c.txt"]));
    ok(run(&old, dir, &["commit", "-m", "second"]));
    same_reads(&old, dir);
    // this build looks at what the older one recorded as synced unseen
    add(dir, "d.txt");
    ok(run(&new, dir, &["commit", "-m", "third"]));
    same_reads(&old, dir);
    assert!(ok(run(&new, dir, &["fsck"])).ends_with("every record verified\n"));
}

/// `b.txt` staged by this build, its blob lost to a power cut and the
/// record that names it left; then `c.txt` staged by the older build,
/// which records all it finds as synced.
pub(super) fn an_older_build_wrote_past_a_lost_blob(old: &Path, dir: &Path) {
    add(dir, "b.txt");
    let alt_dir = dir.join(".alt");
    for (name, len) in FILES.iter().zip(objects_synced(&alt_dir)).take(3) {
        let file = fs::OpenOptions::new().write(true).open(alt_dir.join(name));
        file.unwrap().set_len(len).unwrap();
    }
    another_boot(&alt_dir);
    fs::write(dir.join("c.txt"), "c\n").unwrap();
    ok(run(old, dir, &["add", "c.txt"]));
}

#[test]
#[ignore = "run with ALT_OLD_BIN set to an alt built from the base branch"]
fn a_name_without_content_that_an_older_build_recorded_as_synced_is_reported_and_nothing_cut() {
    let (old, new) = (old_bin(), new_bin());
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    repo(dir);
    an_older_build_wrote_past_a_lost_blob(&old, dir);
    let alt_dir = dir.join(".alt");
    let after_old = lens(&alt_dir);
    fs::write(dir.join("d.txt"), "d\n").unwrap();
    let added = run(&new, dir, &["add", "d.txt"]);
    let said = String::from_utf8_lossy(&added.stderr).into_owned();
    assert!(!added.status.success(), "{said}");
    assert!(
        said.contains("of map.alt names content the store does not hold")
            && said.contains("`alt fsck --drop-lost`"),
        "{said}"
    );
    // what the older build synced after that record is all there
    assert_eq!(lens(&alt_dir), after_old);
    ok(run(&new, dir, &["status"]));
    ok(run(&old, dir, &["status"]));
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
#[ignore = "run with ALT_OLD_BIN set to an alt built from the base branch"]
fn records_naming_a_lost_blob_are_taken_out_on_request_and_both_builds_go_on() {
    let (old, new) = (old_bin(), new_bin());
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    repo(dir);
    an_older_build_wrote_past_a_lost_blob(&old, dir);
    let alt_dir = dir.join(".alt");
    let before = lens(&alt_dir);
    let listed = run(&new, dir, &["fsck", "--drop-lost"]);
    let said = text(&listed.stdout);
    assert_eq!(listed.status.code(), Some(1), "{said}");
    for line in [
        "of map.alt names content that is not stored",
        "goes with them: no other record names it",
        "index entry b.txt names blob",
        "nothing was changed: `alt fsck --drop-lost --yes` takes these 1 record(s) out",
    ] {
        assert!(said.contains(line), "{line}: {said}");
    }
    assert_eq!(lens(&alt_dir), before);
    let taken = ok(run(&new, dir, &["fsck", "--drop-lost", "--yes"]));
    assert!(taken.contains("took 1 record(s) out"), "{taken}");
    assert!(taken.contains("add the files named above again"), "{taken}");
    // one record of map.alt is gone and nothing else of any file
    let after = lens(&alt_dir);
    assert_eq!((&after[..3], after[3]), (&before[..3], before[3] - 82));
    let none = ok(run(&new, dir, &["fsck", "--drop-lost"]));
    assert_eq!(none, "no record names content that is not stored\n");
    // what the older build staged after the lost blob is staged still
    for bin in [&old, &new] {
        let status = ok(run(bin, dir, &["status"]));
        assert!(status.contains("new file:   c.txt"), "{status}");
    }
    fs::write(dir.join("d.txt"), "d\n").unwrap();
    ok(run(&new, dir, &["add", "b.txt", "d.txt"]));
    ok(run(&new, dir, &["commit", "-m", "second"]));
    same_reads(&old, dir);
    assert!(ok(run(&new, dir, &["fsck"])).ends_with("every record verified\n"));
    let tree = head_tree(&new, dir);
    for name in ["a.txt", "b.txt", "c.txt", "d.txt"] {
        assert!(tree.contains(name), "{name}: {tree}");
    }
    fs::write(dir.join("e.txt"), "e\n").unwrap();
    ok(run(&old, dir, &["add", "e.txt"]));
    ok(run(&old, dir, &["commit", "-m", "third"]));
    same_reads(&old, dir);
}

/// Stores a commit on top of `HEAD` whose tree holds `a.txt` and the
/// blob `b.txt` is about to be staged as, which is not stored yet, and
/// tags it `side`.
fn a_commit_names_the_blob_to_come(dir: &Path) {
    use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};

    let blob =
        |text: &str| ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, text.as_bytes());
    let mut tree = Vec::new();
    for (name, oid) in [("a.txt", blob("one\n")), ("b.txt", blob("b.txt\n"))] {
        tree.extend_from_slice(format!("100644 {name}\0").as_bytes());
        tree.extend_from_slice(oid.as_bytes());
    }
    let head = ok(run(&new_bin(), dir, &["rev-parse", "HEAD"]));
    let tree_oid = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Tree, &tree);
    let who = "t <t@e> 0 +0000";
    let commit = format!(
        "tree {tree_oid}\nparent {}\nauthor {who}\ncommitter {who}\n\nside\n",
        head.trim()
    );
    let commit_oid = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Commit, commit.as_bytes());
    let mut odb = alt_odb::NativeOdb::open(dir.join(".alt")).unwrap();
    odb.put(tree_oid, ObjectKind::Tree, &tree).unwrap();
    odb.put(commit_oid, ObjectKind::Commit, commit.as_bytes())
        .unwrap();
    odb.flush().unwrap();
    drop(odb);
    ok(run(
        &new_bin(),
        dir,
        &["tag", "side", &commit_oid.to_string()],
    ));
}

#[test]
#[ignore = "run with ALT_OLD_BIN set to an alt built from the base branch"]
fn records_naming_a_lost_blob_that_a_commit_names_are_taken_out_only_when_that_is_asked_for() {
    let (old, new) = (old_bin(), new_bin());
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    repo(dir);
    a_commit_names_the_blob_to_come(dir);
    an_older_build_wrote_past_a_lost_blob(&old, dir);
    let alt_dir = dir.join(".alt");
    let before = lens(&alt_dir);
    let refused = run(&new, dir, &["fsck", "--drop-lost", "--yes"]);
    let (listed, said) = (text(&refused.stdout), text(&refused.stderr));
    assert!(!refused.status.success(), "{listed}");
    assert!(
        listed.contains("reached from refs/tags/side, names"),
        "{listed}"
    );
    assert!(listed.contains("at b.txt"), "{listed}");
    assert!(said.contains("nothing was changed"), "{said}");
    assert!(said.contains("--committed"), "{said}");
    assert_eq!(lens(&alt_dir), before);
    let taken = ok(run(
        &new,
        dir,
        &["fsck", "--drop-lost", "--yes", "--committed"],
    ));
    assert!(taken.contains("took 1 record(s) out"), "{taken}");
    assert_eq!(lens(&alt_dir)[3], before[3] - 82);
    for bin in [&old, &new] {
        ok(run(bin, dir, &["status"]));
    }
    // every record reads, and the tagged commit names a blob none holds
    let checked = run(&new, dir, &["fsck"]);
    let said = text(&checked.stderr);
    assert!(!checked.status.success(), "{said}");
    assert!(said.contains("fsck: missing blob "), "{said}");
    let by = "reached from refs/tags/side, names it at b.txt; its records were taken out";
    assert!(said.contains(by), "{said}");
    // the file is still in the working tree: added again, its blob is
    // the one the commit names
    ok(run(&new, dir, &["add", "b.txt"]));
    ok(run(&new, dir, &["commit", "-m", "second"]));
    same_reads(&old, dir);
    assert!(ok(run(&new, dir, &["fsck"])).ends_with("every record verified\n"));
}

/// Without the note of the boot the appends were left unsynced in, this
/// build does not look behind what the older one recorded as synced: its
/// commands go on, and an `add .` finds every staged blob named.
#[test]
#[ignore = "run with ALT_OLD_BIN set to an alt built from the base branch"]
fn a_lost_blob_this_build_took_for_stored_is_stored_again_once_its_record_is_taken_out() {
    use std::os::unix::fs::FileExt;

    let (old, new) = (old_bin(), new_bin());
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    repo(dir);
    an_older_build_wrote_past_a_lost_blob(&old, dir);
    let lock = fs::OpenOptions::new()
        .write(true)
        .open(dir.join(".alt/odb.lock"));
    lock.unwrap().write_all_at(&[0; 64], 1224).unwrap();
    // enough staged files for an `add .` to look through all of them
    // at once and note that the blob of each is stored
    for n in 0..40 {
        fs::write(dir.join(format!("e{n}.txt")), format!("e{n}\n")).unwrap();
    }
    ok(run(&old, dir, &["add", "."]));
    ok(run(&new, dir, &["add", "."]));
    let refused = run(&new, dir, &["commit", "-m", "second"]);
    assert!(!refused.status.success(), "{}", text(&refused.stdout));
    let taken = ok(run(&new, dir, &["fsck", "--drop-lost", "--yes"]));
    assert!(taken.contains("took 1 record(s) out"), "{taken}");
    // what said that every staged blob is stored says so no longer
    ok(run(&new, dir, &["add", "."]));
    ok(run(&new, dir, &["commit", "-m", "second"]));
    same_reads(&old, dir);
    let tree = head_tree(&new, dir);
    assert!(tree.contains("b.txt") && tree.contains("c.txt"), "{tree}");
}
