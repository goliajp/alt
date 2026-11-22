//! An `add` brings the snapshot beside the index (`index.scan`) to the
//! index it puts in place with a record at the end of the file, and what
//! the commands after it read from the file with its records is what they
//! make of the index without it.

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use super::scan_daemon::{Stop, daemon_pid, run, write};

const SCAN: &str = ".alt/index.scan";

fn tree(root: &Path) {
    run(root, &["init"], false);
    write(root, "a.txt", "a\n");
    write(root, "src/lib.rs", "lib\n");
    write(root, "src/deep/mod.rs", "mod\n");
    write(root, "docs/guide.md", "guide\n");
    write(root, "docs/more.md", "more\n");
    run(root, &["add", "."], false);
    run(root, &["commit", "-m", "base"], false);
    run(root, &["status"], false);
}

/// What the commands that go by the snapshot say.
fn said(root: &Path, daemon: bool) -> Vec<String> {
    let asked: [&[&str]; 4] = [
        &["status"],
        &["status", "--json"],
        &["diff"],
        &["diff", "--cached"],
    ];
    asked.iter().map(|args| run(root, args, daemon)).collect()
}

/// Stages a changed file, a new one in a new directory and a removal, one
/// `add` each, and has every `add` leave the snapshot's file in place.
fn staged_by_records(root: &Path, daemon: bool) {
    let file = fs::metadata(root.join(SCAN)).unwrap();
    let mut len = file.len();
    let mut add = |path: &str| {
        run(root, &["add", path], daemon);
        let now = fs::metadata(root.join(SCAN)).unwrap();
        assert_eq!(now.ino(), file.ino(), "{path}: the file was written anew");
        assert!(now.len() > len, "{path}: nothing was added to the file");
        len = now.len();
    };
    write(root, "src/lib.rs", "lib changed\n");
    add("src/lib.rs");
    write(root, "new/dir/file.txt", "new\n");
    add("new/dir/file.txt");
    fs::remove_file(root.join("docs/guide.md")).unwrap();
    add("docs/guide.md");
    assert!(
        len < file.len() + 2000,
        "three records: {} bytes",
        len - file.len()
    );
}

fn the_snapshot_with_records_says_what_one_made_anew_says(daemon: bool) {
    let tmp = alt_os::temp::tempdir().unwrap();
    let root = tmp.path();
    tree(root);
    let _stop = daemon.then(|| {
        run(root, &["status"], true);
        Stop(daemon_pid(root), false)
    });
    staged_by_records(root, daemon);
    // a file changed and not staged, for the stats to tell
    write(root, "a.txt", "a changed\n");
    let with = said(root, daemon);
    for line in [
        "modified:   src/lib.rs",
        "new file:   new/dir/file.txt",
        "deleted:    docs/guide.md",
        "modified:   a.txt",
    ] {
        assert!(with[0].contains(line), "{line}: {}", with[0]);
    }
    assert_eq!(with, said(root, false));
    fs::remove_file(root.join(SCAN)).unwrap();
    assert_eq!(with, said(root, false));
    run(root, &["commit", "-m", "second"], daemon);
    assert_eq!(with[2], run(root, &["diff"], false));
    assert!(run(root, &["status"], false).contains("modified:   a.txt"));
    assert!(run(root, &["fsck"], false).ends_with("every record verified\n"));
}

#[test]
fn the_snapshot_an_add_adds_a_record_to_says_what_one_made_anew_says() {
    the_snapshot_with_records_says_what_one_made_anew_says(false);
}

#[test]
fn a_daemon_adds_its_records_and_goes_on_from_what_it_holds() {
    the_snapshot_with_records_says_what_one_made_anew_says(true);
}

#[test]
fn a_record_cut_short_is_passed_over_and_the_file_written_anew() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let root = tmp.path();
    tree(root);
    staged_by_records(root, false);
    let whole = fs::read(root.join(SCAN)).unwrap();
    let before = said(root, false);
    // the last record did not reach the disk to its end
    crate::replace_file(&root.join(SCAN), &whole[..whole.len() - 9]);
    assert_eq!(said(root, false), before);
    write(root, "src/lib.rs", "lib changed again\n");
    run(root, &["add", "src/lib.rs"], false);
    let status = run(root, &["status"], false);
    assert!(status.contains("modified:   src/lib.rs"), "{status}");
    assert!(status.contains("deleted:    docs/guide.md"), "{status}");
    let after = said(root, false);
    fs::remove_file(root.join(SCAN)).unwrap();
    assert_eq!(said(root, false), after);
}

mod mixed_versions {
    use super::super::mixed_versions::{new_bin, old_bin, same_reads};
    use super::*;

    #[test]
    #[ignore = "run with ALT_OLD_BIN set to an alt built from the base branch"]
    fn an_older_build_passes_over_a_snapshot_with_records_and_this_one_adds_to_what_it_leaves() {
        let (old, new) = (old_bin(), new_bin());
        let tmp = alt_os::temp::tempdir().unwrap();
        let root = tmp.path();
        tree(root);
        let by = |bin: &Path, args: &[&str]| {
            let out = super::super::mixed_versions::run(bin, root, args);
            assert!(out.status.success(), "{args:?}: {out:?}");
        };
        staged_by_records(root, false);
        same_reads(&old, root);
        // the older build makes a file of its own, without records
        write(root, "a.txt", "a by the older build\n");
        by(&old, &["add", "a.txt"]);
        same_reads(&old, root);
        let whole = fs::metadata(root.join(SCAN)).unwrap();
        write(root, "src/deep/mod.rs", "mod by this build\n");
        by(&new, &["add", "src/deep/mod.rs"]);
        let added = fs::metadata(root.join(SCAN)).unwrap();
        assert_eq!(added.ino(), whole.ino());
        assert!(added.len() > whole.len());
        same_reads(&old, root);
        by(&old, &["commit", "-m", "by the older build"]);
        same_reads(&old, root);
        write(root, "a.txt", "a once more\n");
        by(&new, &["add", "a.txt"]);
        by(&new, &["commit", "-m", "by this build"]);
        same_reads(&old, root);
    }
}
