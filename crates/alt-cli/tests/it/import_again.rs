//! An import into a store that holds adds which were left unsynced
//! records their operations as synced when it moves a ref: it syncs what
//! those adds wrote first, the index and its snapshots among it.

use std::fs;
use std::path::Path;
use std::process::Command;

use super::staged_durability::synced_to_the_ends;
use super::synced_files::{library, synced};
use super::unconfirmed::{alt, alt_command, ok};

fn git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .env("GIT_COMMITTER_NAME", "tester")
        .env("GIT_COMMITTER_EMAIL", "t@e")
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "{args:?}");
}

fn snapshots(dir: &Path) -> Vec<String> {
    let listing = fs::read_dir(dir.join(".alt/recovery/objects"));
    let names = listing.map(|entries| {
        entries
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect()
    });
    names.unwrap_or_default()
}

#[test]
fn an_import_that_moves_a_ref_syncs_what_adds_left_unsynced_before_it_records_them() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let (src, dst) = (&tmp.path().join("src"), &tmp.path().join("dst"));
    fs::create_dir(src).unwrap();
    git(src, &["init", "-q", "-b", "main"]);
    fs::write(src.join("a.txt"), "one\n").unwrap();
    git(src, &["add", "a.txt"]);
    git(src, &["commit", "-qm", "first"]);
    ok(alt(src, &["import", dst.to_str().unwrap()]));
    let before = snapshots(dst);
    fs::write(dst.join("n.txt"), "staged\n").unwrap();
    ok(alt(dst, &["add", "n.txt"]));
    assert!(!synced_to_the_ends(dst));
    let written: Vec<String> = snapshots(dst)
        .into_iter()
        .filter(|name| !before.contains(name))
        .collect();
    assert!(!written.is_empty());

    fs::write(src.join("a.txt"), "two\n").unwrap();
    git(src, &["commit", "-qam", "second"]);
    let library = library(tmp.path());
    let again = alt_command(src, &["import", dst.to_str().unwrap()]);
    let (out, synced) = synced(dst, &library, again);
    ok(out);
    assert!(synced_to_the_ends(dst));
    // the index, which is one file with the snapshot the add left
    use std::os::unix::fs::MetadataExt;
    assert!(synced.contains(&".alt/index".to_owned()), "{synced:?}");
    let index = fs::metadata(dst.join(".alt/index")).unwrap().ino();
    for name in written {
        let snapshot = dst.join(".alt/recovery/objects").join(name);
        assert_eq!(fs::metadata(snapshot).unwrap().ino(), index, "{synced:?}");
    }
    // the add is as it was
    assert!(ok(alt(dst, &["status"])).contains("new file:   n.txt"));
    ok(alt(dst, &["fsck"]));
}
