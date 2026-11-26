//! What a fetch leaves in the directory of packs of the repository.

use std::path::Path;
use std::time::{Duration, SystemTime};

use super::held::git;

/// A repository of git with one commit, served by the shim, and a clone of
/// it in `dir/copy`.
fn cloned(dir: &Path) -> std::path::PathBuf {
    let repo = dir.join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    committed(&repo, "one");
    let url = super::spawn(repo);
    crate::altd_server::ok(crate::altd_server::alt(dir, &["clone", &url, "copy"]));
    dir.join("copy")
}

/// A commit of a new file `name` in the repository of git at `repo`.
fn committed(repo: &Path, name: &str) {
    std::fs::write(repo.join(name), name).unwrap();
    git(repo, &["add", name]);
    let who = ["-c", "user.name=t", "-c", "user.email=t@e"];
    git(repo, &[&who[..], &["commit", "-qm", name]].concat());
}

/// A temporary file that a fetch ended long ago left behind is removed by
/// the next fetch; one that a fetch going on now writes to is not
#[test]
fn a_fetch_removes_the_temporary_files_that_fetches_ended_long_ago_left() {
    let dir = alt_os::temp::tempdir().unwrap();
    let copy = cloned(dir.path());
    committed(&dir.path().join("repo"), "two");
    let packs = copy.join(".alt/pack");
    let (old, new) = (
        packs.join(".tmpOldOldOldOld"),
        packs.join(".tmpNewNewNewNew"),
    );
    std::fs::write(&old, "left").unwrap();
    std::fs::write(&new, "being written").unwrap();
    let long_ago = SystemTime::now() - Duration::from_secs(3 * 60 * 60);
    let file = std::fs::File::options().write(true).open(&old).unwrap();
    file.set_modified(long_ago).unwrap();
    drop(file);

    crate::altd_server::ok(crate::altd_server::alt(&copy, &["fetch", "origin"]));
    assert!(!old.exists());
    assert_eq!(std::fs::read_to_string(&new).unwrap(), "being written");
}
