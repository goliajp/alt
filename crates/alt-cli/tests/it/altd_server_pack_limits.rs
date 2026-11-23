//! What the pack of a push may inflate to: no object to more than a push
//! may bring, and all of them together to no more than the server is
//! told. A pack is as its sender made it, and a few bytes of it can
//! stand for any number.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::altd_server::{alt, ok, spawn_server_with_env};

pub(crate) fn git(dir: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(["-c", "user.name=tester", "-c", "user.email=t@e"])
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .output()
        .unwrap()
}

/// A git repository beneath `root` with one commit of its own.
pub(crate) fn source(root: &Path, label: &str) -> PathBuf {
    let src = root.join(label);
    fs::create_dir(&src).unwrap();
    assert!(
        git(&src, &["init", "-q", "-b", "main", "."])
            .status
            .success()
    );
    fs::write(src.join("f.txt"), format!("{label}\n")).unwrap();
    assert!(git(&src, &["add", "."]).status.success());
    assert!(git(&src, &["commit", "-q", "-m", label]).status.success());
    src
}

pub(crate) fn push(src: &Path, url: &str, branch: &str) -> Output {
    git(src, &["push", url, &format!("HEAD:refs/heads/{branch}")])
}

/// A commit in `src` of `count` files, each `bytes` of one letter.
fn commit_of(src: &Path, count: usize, bytes: usize) {
    for n in 0..count {
        let letter = [b'a' + (n % 26) as u8];
        fs::write(src.join(format!("{n}.txt")), letter.repeat(bytes + n)).unwrap();
    }
    assert!(git(src, &["add", "."]).status.success());
    assert!(git(src, &["commit", "-q", "-m", "files"]).status.success());
}

#[test]
fn a_push_whose_objects_inflate_past_the_limits_is_refused() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    fs::create_dir(&origin).unwrap();
    ok(alt(&origin, &["init", "."]));
    fs::write(origin.join("seed.txt"), "seed\n").unwrap();
    ok(alt(&origin, &["add", "."]));
    ok(alt(&origin, &["commit", "-m", "seed"]));
    // sixteen times what a push may bring, all together
    let server = spawn_server_with_env(&[
        ("ALT_SERVER_REPO", origin.to_str().unwrap()),
        ("ALT_SERVER_MAX_PUSH_BYTES", "8192"),
    ]);
    let url = format!("http://{}/", server.addr);
    let said = |out: &Output| String::from_utf8_lossy(&out.stderr).into_owned();

    // one file of a hundred thousand letters is a pack of a few hundred bytes
    let one = source(tmp.path(), "one");
    commit_of(&one, 1, 100_000);
    let refused = push(&one, &url, "one");
    assert!(!refused.status.success(), "{}", said(&refused));
    let why = "pack resource limit: object bytes";
    assert!(said(&refused).contains(why), "{}", said(&refused));

    // fifty files of four thousand: each fits, together they do not
    let many = source(tmp.path(), "many");
    commit_of(&many, 50, 4000);
    let refused = push(&many, &url, "many");
    assert!(!refused.status.success(), "{}", said(&refused));
    let why = "pack resource limit: inflated bytes";
    assert!(said(&refused).contains(why), "{}", said(&refused));

    let few = source(tmp.path(), "few");
    commit_of(&few, 5, 4000);
    let landed = push(&few, &url, "few");
    assert!(landed.status.success(), "{}", said(&landed));
    let listed = git(tmp.path(), &["ls-remote", &url]);
    let listed = String::from_utf8_lossy(&listed.stdout).into_owned();
    assert!(listed.contains("refs/heads/few"), "{listed}");
    assert!(!listed.contains("refs/heads/one"), "{listed}");
    assert!(!listed.contains("refs/heads/many"), "{listed}");
}
