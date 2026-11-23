//! A sync of a group of writes that fails, in a server and in a daemon:
//! the kernel reports the failure of an fsync once and answers the next
//! fsync of the same file with success, the data not being on disk. No
//! write is told it is durable on the strength of such a second fsync,
//! and none is taken in behind the one that was not synced.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::altd_server::{alt, ok, spawn_server_with_env};
use crate::daemon_stop::Stop;
use crate::scan_daemon::daemon_pid;

/// A library that fails one `fsync` of a file whose path ends with what
/// `ALT_TEST_FAIL_SYNC_OF` holds, once the file `ALT_TEST_FAIL_SYNC_ARMED`
/// names is there, and takes that file away: every later fsync is made
/// and answers as the kernel does.
fn failing_once(root: &Path) -> PathBuf {
    let source = root.join("fails-a-sync.c");
    let library = root.join("fails-a-sync.so");
    fs::write(
        &source,
        r#"
#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
static int fails(int fd) {
    const char *of = getenv("ALT_TEST_FAIL_SYNC_OF");
    const char *armed = getenv("ALT_TEST_FAIL_SYNC_ARMED");
    char link[64], path[4096];
    if (!of || !armed) return 0;
    snprintf(link, sizeof link, "/proc/self/fd/%d", fd);
    ssize_t n = readlink(link, path, sizeof path - 1);
    size_t m = strlen(of);
    if (n < 0 || (size_t)n < m || memcmp(path + n - m, of, m) != 0) return 0;
    return unlink(armed) == 0;
}
int fsync(int fd) {
    if (fails(fd)) { errno = EIO; return -1; }
    int (*real)(int) = dlsym(RTLD_NEXT, "fsync");
    return real(fd);
}
"#,
    )
    .unwrap();
    let built = Command::new("cc")
        .args(["-shared", "-fPIC", "-O2", "-o"])
        .arg(&library)
        .arg(&source)
        .arg("-ldl")
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    library
}

fn git(dir: &Path, args: &[&str]) -> Output {
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
fn source(root: &Path, label: &str) -> PathBuf {
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

fn push(src: &Path, url: &str, branch: &str) -> Output {
    git(src, &["push", url, &format!("HEAD:refs/heads/{branch}")])
}

#[test]
fn no_push_is_told_it_landed_after_a_sync_that_failed() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    fs::create_dir(&origin).unwrap();
    ok(alt(&origin, &["init", "."]));
    fs::write(origin.join("seed.txt"), "seed\n").unwrap();
    ok(alt(&origin, &["add", "."]));
    ok(alt(&origin, &["commit", "-m", "seed"]));

    let library = failing_once(tmp.path());
    let armed = tmp.path().join("armed");
    let server = spawn_server_with_env(&[
        ("ALT_SERVER_REPO", origin.to_str().unwrap()),
        ("LD_PRELOAD", library.to_str().unwrap()),
        ("ALT_TEST_FAIL_SYNC_OF", "/oplog/log"),
        ("ALT_TEST_FAIL_SYNC_ARMED", armed.to_str().unwrap()),
    ]);
    let url = format!("http://{}/", server.addr);
    let said = |out: &Output| String::from_utf8_lossy(&out.stderr).into_owned();

    let before = push(&source(tmp.path(), "before"), &url, "before");
    assert!(before.status.success(), "{}", said(&before));

    fs::write(&armed, "").unwrap();
    let failed = push(&source(tmp.path(), "failed"), &url, "failed");
    assert!(!armed.exists(), "no sync of the log was made");
    assert!(!failed.status.success(), "{}", said(&failed));
    assert!(said(&failed).contains("durability"), "{}", said(&failed));

    // its ref transaction lies behind the one that was not synced
    let after = push(&source(tmp.path(), "after"), &url, "after");
    assert!(!after.status.success(), "{}", said(&after));
    assert!(
        said(&after).contains("writes are stopped"),
        "{}",
        said(&after)
    );
    // and nothing of it was taken in: the server reads on
    let listed = git(tmp.path(), &["ls-remote", &url]);
    assert!(listed.status.success(), "{}", said(&listed));
    let listed = String::from_utf8_lossy(&listed.stdout).into_owned();
    assert!(listed.contains("refs/heads/before"), "{listed}");
    assert!(!listed.contains("refs/heads/after"), "{listed}");
}

/// Runs `alt` in `repo` through its daemon, which is started with the
/// library preloaded when there is none.
fn through_daemon(repo: &Path, library: &Path, armed: &Path, args: &[&str]) -> Output {
    crate::alt_command()
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .env("XDG_CONFIG_HOME", repo.join(".alt/test-config"))
        .env_remove("ALT_NO_DAEMON")
        .env("ALT_DAEMON_IDLE_MS", "60000")
        .env("LD_PRELOAD", library)
        .env("ALT_TEST_FAIL_SYNC_OF", "/oplog/log")
        .env("ALT_TEST_FAIL_SYNC_ARMED", armed)
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn a_daemon_refuses_to_write_after_a_sync_that_failed() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path().join("repo");
    fs::create_dir(&dir).unwrap();
    crate::staged_durability::repo(&dir);
    let library = failing_once(tmp.path());
    let armed = tmp.path().join("armed");
    let run = |args: &[&str]| through_daemon(&dir, &library, &armed, args);
    let said = |out: &Output| String::from_utf8_lossy(&out.stderr).into_owned();

    let before = run(&["branch", "before"]);
    assert!(before.status.success(), "{}", said(&before));
    let _stop = Stop(daemon_pid(&dir), false);

    fs::write(&armed, "").unwrap();
    let failed = run(&["branch", "failed"]);
    assert!(!armed.exists(), "no sync of the log was made");
    assert!(!failed.status.success(), "{}", said(&failed));
    assert!(said(&failed).contains("durability"), "{}", said(&failed));

    let after = run(&["branch", "after"]);
    assert!(!after.status.success(), "{}", said(&after));
    assert!(
        said(&after).contains("writes are stopped"),
        "{}",
        said(&after)
    );
    // nothing of it was appended, and the daemon reads on
    let listed = run(&["branch"]);
    assert!(listed.status.success(), "{}", said(&listed));
    let listed = String::from_utf8_lossy(&listed.stdout).into_owned();
    assert!(listed.contains("before"), "{listed}");
    assert!(!listed.contains("after"), "{listed}");
}
