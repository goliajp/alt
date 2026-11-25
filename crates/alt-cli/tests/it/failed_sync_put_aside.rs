//! A sync fails in a push whose repository was put aside while the push had it,
//! and the push is the last to have the repository's writer: the failure is known
//! of the repository when it is put back.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use crate::altd_server::spawn_server_with_env;
use crate::altd_server_pack_limits::{git, push, source};
use crate::altd_server_untrusted::repository;

/// A library whose `fsync` of a file whose path ends with what
/// `ALT_TEST_FAIL_SYNC_OF` holds, once the file `ALT_TEST_FAIL_SYNC_ARMED` names is
/// there, first puts the directory `ALT_TEST_ASIDE_FROM` at `ALT_TEST_ASIDE_TO`
/// and the directory `ALT_TEST_INSTEAD` in its place, waits for the file
/// `ALT_TEST_GO_ON` to be there, and then fails.
fn failing_once_aside(root: &Path) -> PathBuf {
    let source = root.join("fails-a-sync-aside.c");
    let library = root.join("fails-a-sync-aside.so");
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
    if (unlink(armed) != 0) return 0;
    rename(getenv("ALT_TEST_ASIDE_FROM"), getenv("ALT_TEST_ASIDE_TO"));
    rename(getenv("ALT_TEST_INSTEAD"), getenv("ALT_TEST_ASIDE_FROM"));
    for (int waited = 0; access(getenv("ALT_TEST_GO_ON"), F_OK) != 0 && waited < 6000; waited++)
        usleep(10000);
    return 1;
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
    assert!(built.status.success());
    library
}

fn said(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn a_sync_that_failed_in_the_last_push_of_a_repository_put_aside_is_known_when_it_is_put_back() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let root = tmp.path();
    let (served, aside) = (root.join("served"), root.join("aside"));
    let (instead, armed, go_on) = (root.join("instead"), root.join("armed"), root.join("go"));
    repository(&served.join("repo"));
    repository(&instead);
    let library = failing_once_aside(root);
    let server = spawn_server_with_env(&[
        ("ALT_SERVER_ROOT", served.to_str().unwrap()),
        ("LD_PRELOAD", library.to_str().unwrap()),
        ("ALT_TEST_FAIL_SYNC_OF", "/oplog/log"),
        ("ALT_TEST_FAIL_SYNC_ARMED", armed.to_str().unwrap()),
        ("ALT_TEST_ASIDE_FROM", served.join("repo").to_str().unwrap()),
        ("ALT_TEST_ASIDE_TO", aside.to_str().unwrap()),
        ("ALT_TEST_INSTEAD", instead.to_str().unwrap()),
        ("ALT_TEST_GO_ON", go_on.to_str().unwrap()),
    ]);
    let url = format!("http://{}/repo/", server.addr);
    let before = push(&source(root, "before"), &url, "before");
    assert!(before.status.success(), "{}", said(&before));

    // the push has the writer of the repository when its sync is come to: the
    // repository is put aside there, and another is under its name
    fs::write(&armed, "").unwrap();
    let failing = source(root, "failed");
    let pushing = std::thread::spawn({
        let url = url.clone();
        move || push(&failing, &url, "failed")
    });
    let until = Instant::now() + Duration::from_secs(60);
    while !aside.exists() || instead.exists() {
        assert!(Instant::now() < until, "no sync of the log was come to");
        std::thread::sleep(Duration::from_millis(20));
    }
    // a request for the name meets the other repository: the server lets go of the
    // one that was put aside, whose push is still under way and has not failed yet
    let listed = git(root, &["ls-remote", &url]);
    assert!(listed.status.success(), "{}", said(&listed));
    let listed = String::from_utf8_lossy(&listed.stdout).into_owned();
    assert!(!listed.contains("refs/heads/before"), "{listed}");
    // now the sync fails, in the last request that has the writer
    fs::write(&go_on, "").unwrap();
    let failed = pushing.join().unwrap();
    assert!(!failed.status.success(), "{}", said(&failed));

    // put back, the repository takes no write
    fs::rename(served.join("repo"), &instead).unwrap();
    fs::rename(&aside, served.join("repo")).unwrap();
    let after = push(&source(root, "after"), &url, "after");
    assert!(!after.status.success(), "{}", said(&after));
    let stopped = said(&after).contains("writes are stopped");
    assert!(stopped, "{}", said(&after));
    let listed = git(root, &["ls-remote", &url]);
    let listed = String::from_utf8_lossy(&listed.stdout).into_owned();
    assert!(listed.contains("refs/heads/before"), "{listed}");
    assert!(!listed.contains("refs/heads/after"), "{listed}");
}
