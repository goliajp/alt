//! A store this build imported, used by an older program that reads the
//! store's own configuration file and knows of no copy beside it: it
//! takes the objects for what they are hashed with, and reads the
//! settings the git repository had, the one for line ends among them.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use super::{new_bin, ok, old_bin};

/// `bin` with nobody named by the environment: who commits is what the
/// configuration says. Served by the daemon beside `bin` where `daemon`
/// says so.
fn alt(bin: &Path, daemon: bool, repo: &Path, args: &[&str]) -> Output {
    let mut alt = Command::new(bin);
    alt.env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("TZ", "UTC");
    for name in [
        "GIT_AUTHOR_NAME",
        "GIT_AUTHOR_EMAIL",
        "GIT_COMMITTER_NAME",
        "GIT_COMMITTER_EMAIL",
    ] {
        alt.env_remove(name);
    }
    match daemon {
        true => alt.env("ALT_DAEMON_IDLE_MS", "1500"),
        false => alt.env("ALT_NO_DAEMON", "1"),
    };
    alt.current_dir(repo).args(args).output().unwrap()
}

fn git(repo: &Path, args: &[&str]) -> String {
    let run = Command::new("git")
        .current_dir(repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args(args)
        .output()
        .unwrap();
    ok(run)
}

/// A git repository of `format` in `dir` that names who commits and
/// converts line ends, with a commit on `main` and one more on `side`.
fn source(dir: &Path, format: &str) -> PathBuf {
    let source = dir.join("git");
    std::fs::create_dir(&source).unwrap();
    let hashed = format!("--object-format={format}");
    git(&source, &["init", "-q", "-b", "main", &hashed, "."]);
    for (key, value) in [
        ("user.name", "Source User"),
        ("user.email", "source@user.invalid"),
        ("core.autocrlf", "input"),
    ] {
        git(&source, &["config", key, value]);
    }
    std::fs::write(source.join("f"), "one\n").unwrap();
    git(&source, &["add", "f"]);
    git(&source, &["commit", "-q", "-m", "one"]);
    git(&source, &["switch", "-q", "-c", "side"]);
    std::fs::write(source.join("g"), "two\n").unwrap();
    git(&source, &["add", "g"]);
    git(&source, &["commit", "-q", "-m", "two"]);
    git(&source, &["switch", "-q", "main"]);
    source
}

fn an_older_program_works_in_a_store_imported_since(format: &str, width: usize) {
    let (old, new) = (old_bin(), new_bin());
    let dir = alt_os::temp::tempdir().unwrap();
    let (source, store) = (source(dir.path(), format), dir.path().join("alt"));
    let target = store.to_str().unwrap();
    ok(alt(&new, false, &source, &["import", target]));
    let own = store.join(".alt/git-import/config");
    let led = std::fs::read(&own).unwrap();

    // the older program reads what the git repository read
    let read = |bin: &Path, args: &[&str]| ok(alt(bin, false, &store, args));
    for bin in [&old, &new] {
        assert_eq!(read(bin, &["config", "user.name"]), "Source User\n");
        assert_eq!(read(bin, &["config", "core.autocrlf"]), "input\n");
        let head = read(bin, &["rev-parse", "HEAD"]);
        assert_eq!(head.trim().len(), width, "{head}");
        assert_eq!(head, git(&source, &["rev-parse", "HEAD"]));
    }
    assert_eq!(read(&old, &["status"]), read(&new, &["status"]));

    // and writes as the store is laid out, by the daemon of its own too
    std::fs::write(store.join("h"), "three\r\n").unwrap();
    read(&old, &["add", "h"]);
    read(&old, &["commit", "-m", "three"]);
    read(&old, &["switch", "side"]);
    read(&old, &["switch", "main"]);
    read(&old, &["merge", "side"]);
    std::fs::write(store.join("i"), "four\r\n").unwrap();
    ok(alt(&old, true, &store, &["add", "i"]));
    ok(alt(&old, true, &store, &["commit", "-m", "four"]));

    assert_eq!(read(&new, &["cat-file", "-p", "HEAD:h"]), "three\n");
    assert_eq!(read(&new, &["cat-file", "-p", "HEAD:i"]), "four\n");
    assert_eq!(read(&new, &["cat-file", "-p", "HEAD:g"]), "two\n");
    assert_eq!(read(&new, &["rev-parse", "HEAD"]).trim().len(), width);
    read(&new, &["op-log", "--verify"]);
    let clean = read(&new, &["status"]);
    assert!(clean.contains("nothing to commit"), "{clean}");

    // git takes the export for a repository of the same hash and people
    let exported = dir.path().join("exported");
    read(&new, &["export", exported.to_str().unwrap()]);
    git(&exported, &["fsck", "--strict"]);
    let theirs = git(&exported, &["rev-parse", "--show-object-format"]);
    assert_eq!(theirs.trim(), format);
    assert_eq!(
        git(&exported, &["rev-parse", "HEAD"]),
        read(&new, &["rev-parse", "HEAD"])
    );
    assert_eq!(
        std::fs::read(exported.join(".git/config")).unwrap(),
        std::fs::read(source.join(".git/config")).unwrap()
    );

    // what the older program sets is the store's own, under the head
    // that leads to the copy, and the next import leaves all of it
    read(&old, &["config", "user.name", "Old Set"]);
    assert_eq!(read(&new, &["config", "user.name"]), "Old Set\n");
    assert_eq!(
        read(&old, &["config", "user.email"]),
        "source@user.invalid\n"
    );
    let set = std::fs::read(&own).unwrap();
    assert!(set.starts_with(&led[..led.len().min(32)]), "{set:?}");
    git(&source, &["config", "user.email", "two@user.invalid"]);
    ok(alt(&new, false, &source, &["import", target]));
    assert_eq!(std::fs::read(&own).unwrap(), set);
    for bin in [&old, &new] {
        assert_eq!(read(bin, &["config", "user.name"]), "Old Set\n");
        assert_eq!(read(bin, &["config", "user.email"]), "two@user.invalid\n");
    }
}

#[test]
#[ignore = "run with ALT_OLD_BIN set to an alt built from the base branch"]
fn an_older_program_works_in_a_sha256_store_imported_since() {
    an_older_program_works_in_a_store_imported_since("sha256", 64);
}

#[test]
#[ignore = "run with ALT_OLD_BIN set to an alt built from the base branch"]
fn an_older_program_works_in_a_sha1_store_imported_since() {
    an_older_program_works_in_a_store_imported_since("sha1", 40);
}

#[test]
fn a_variable_of_the_format_is_not_set_by_hand() {
    let new = new_bin();
    let dir = alt_os::temp::tempdir().unwrap();
    let (source, store) = (source(dir.path(), "sha256"), dir.path().join("alt"));
    ok(alt(
        &new,
        false,
        &source,
        &["import", store.to_str().unwrap()],
    ));
    let own = store.join(".alt/git-import/config");
    let before = std::fs::read(&own).unwrap();
    for args in [
        &["config", "extensions.objectformat", "sha1"][..],
        &["config", "--unset", "extensions.objectformat"],
        &["config", "core.repositoryformatversion", "0"],
    ] {
        let refused = alt(&new, false, &store, args);
        assert!(!refused.status.success(), "{args:?}");
        let said = String::from_utf8_lossy(&refused.stderr).into_owned();
        assert!(said.contains("tells the format of the store"), "{said}");
    }
    assert_eq!(std::fs::read(&own).unwrap(), before);
    let list = ok(alt(&new, false, &store, &["config", "--list"]));
    assert_eq!(list, git(&source, &["config", "--local", "--list"]));
}
