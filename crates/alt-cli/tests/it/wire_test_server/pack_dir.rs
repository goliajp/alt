//! What a fetch leaves in the directory of packs of the repository.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
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

/// The files of a pack a clone takes in have the permissions the umask of
/// the clone gives a file, as Git's have
#[test]
fn a_pack_a_fetch_takes_in_has_the_permissions_the_umask_gives() {
    let dir = alt_os::temp::tempdir().unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    committed(&repo, "one");
    let url = super::spawn(repo);
    let alt = crate::alt_command();
    for (umask, mode) in [("077", 0o600), ("022", 0o644)] {
        let copy = format!("copy-{umask}");
        let cloned = Command::new("sh")
            .current_dir(dir.path())
            .args(["-c", &format!("umask {umask} && exec \"$0\" \"$@\"")])
            .arg(alt.get_program())
            .args(["clone", &url, &copy])
            .envs(alt.get_envs().map(|(k, v)| (k, v.unwrap())))
            .env("ALT_NO_DAEMON", "1")
            .output()
            .unwrap();
        crate::altd_server::ok(cloned);
        let packs = dir.path().join(&copy).join(".alt/pack");
        let names: Vec<_> = std::fs::read_dir(&packs)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert!(!names.is_empty());
        for name in names {
            let had = std::fs::metadata(packs.join(&name)).unwrap();
            assert_eq!(had.permissions().mode() & 0o777, mode, "{umask} {name:?}");
        }
    }
}
