//! What a version 2 log does to its file, call by call, seen from a
//! library preloaded into a child of this test.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use alt_oplog::v2::frame::KIND_CHECKPOINT;
use alt_oplog::{Op, OpLog, OpLogError};

use super::v2_log::{boot, file_len, frames, grow, new_log};

/// A library that notes each `pwrite`, `ftruncate` and `fsync` of the
/// file whose path ends as `ALT_TEST_WATCH` says in the file
/// `ALT_TEST_CALLS` names, and makes an `fsync` of it fail for as long
/// as the file `ALT_TEST_FAIL` names is there.
pub fn library(root: &Path) -> PathBuf {
    let (source, library) = (root.join("calls.c"), root.join("calls.so"));
    fs::write(
        &source,
        r#"
#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
static int watched(int fd) {
    const char *want = getenv("ALT_TEST_WATCH");
    char link[64], path[4096];
    if (!want) return 0;
    snprintf(link, sizeof link, "/proc/self/fd/%d", fd);
    ssize_t n = readlink(link, path, sizeof path - 1);
    size_t w = strlen(want);
    if (n < 0 || (size_t)n < w) return 0;
    path[n] = 0;
    return strcmp(path + n - w, want) == 0;
}
static void note(const char *what, long long at, long long len) {
    const char *log = getenv("ALT_TEST_CALLS");
    char line[96];
    if (!log) return;
    int n = snprintf(line, sizeof line, "%s %lld %lld\n", what, at, len);
    int out = open(log, O_WRONLY | O_APPEND | O_CREAT, 0644);
    if (out >= 0) { (void)!write(out, line, n); close(out); }
}
int fsync(int fd) {
    if (watched(fd)) {
        const char *fail = getenv("ALT_TEST_FAIL");
        if (fail && access(fail, F_OK) == 0) { note("fsync-failed", 0, 0); errno = EIO; return -1; }
        note("fsync", 0, 0);
    }
    int (*real)(int) = dlsym(RTLD_NEXT, "fsync");
    return real(fd);
}
ssize_t pwrite64(int fd, const void *buf, size_t len, off64_t at) {
    if (watched(fd)) note("pwrite", at, len);
    ssize_t (*real)(int, const void *, size_t, off64_t) = dlsym(RTLD_NEXT, "pwrite64");
    return real(fd, buf, len, at);
}
ssize_t pwrite(int fd, const void *buf, size_t len, off_t at) {
    if (watched(fd)) note("pwrite", at, len);
    ssize_t (*real)(int, const void *, size_t, off_t) = dlsym(RTLD_NEXT, "pwrite");
    return real(fd, buf, len, at);
}
int ftruncate64(int fd, off64_t len) {
    if (watched(fd)) note("truncate", len, 0);
    int (*real)(int, off64_t) = dlsym(RTLD_NEXT, "ftruncate64");
    return real(fd, len);
}
int ftruncate(int fd, off_t len) {
    if (watched(fd)) note("truncate", len, 0);
    int (*real)(int, off_t) = dlsym(RTLD_NEXT, "ftruncate");
    return real(fd, len);
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
    let said = String::from_utf8_lossy(&built.stderr);
    assert!(built.status.success(), "{said}");
    library
}

/// Runs `scenario` of [`child`] on the log in `dir` with the library
/// preloaded, and returns the calls it made on the log file.
pub fn in_child(dir: &Path, scenario: &str) -> Vec<(String, u64, u64)> {
    let root = alt_os::temp::tempdir().unwrap();
    let calls = root.path().join("calls");
    let ran = Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "v2_faults::child", "--nocapture"])
        .env("LD_PRELOAD", library(root.path()))
        .env("ALT_TEST_WATCH", "/log")
        .env("ALT_TEST_CALLS", &calls)
        .env("ALT_TEST_FAIL", dir.join("fail"))
        .env("ALT_TEST_SCENARIO", scenario)
        .env("ALT_TEST_DIR", dir)
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&ran.stderr);
    assert!(ran.status.success(), "{scenario}: {said}");
    let noted = fs::read_to_string(&calls).unwrap_or_default();
    let call = |line: &str| {
        let mut words = line.split(' ');
        let what = words.next().unwrap().to_owned();
        let mut number = || words.next().unwrap().parse().unwrap();
        (what, number(), number())
    };
    noted.lines().map(call).collect()
}

fn but_gone(op: &Op) -> Result<bool, OpLogError> {
    Ok(op.payload != b"gone")
}

/// The scenario `ALT_TEST_SCENARIO` names, on the log in `ALT_TEST_DIR`:
/// nothing without them, as in a sweep of the ignored tests.
#[test]
#[ignore = "the child the tests of this file run with a library preloaded"]
fn child() {
    let Ok(scenario) = std::env::var("ALT_TEST_SCENARIO") else {
        return;
    };
    let dir = PathBuf::from(std::env::var("ALT_TEST_DIR").unwrap());
    boot(2);
    match scenario.as_str() {
        "cut" => {
            let mut log = OpLog::open(&dir).unwrap();
            assert_eq!(log.settle(but_gone).unwrap().unwrap().ops, 2);
        }
        other => panic!("no scenario {other}"),
    }
}

/// In boot 1: three synced ops, then `gone` and one more left unsynced.
/// Returns where `gone` starts.
fn two_to_cut(dir: &Path) -> u64 {
    boot(1);
    let mut log = new_log(dir);
    grow(&mut log, "a", 3);
    log.append("a", 4, b"gone").unwrap();
    let at = log.anchor().unwrap().start;
    log.append("a", 5, b"after it").unwrap();
    at
}

#[test]
fn a_cut_says_what_it_cuts_in_the_log_and_syncs_that_before_the_file_is_made_shorter() {
    let dir = alt_os::temp::tempdir().unwrap();
    let at = two_to_cut(dir.path());
    let calls = in_child(dir.path(), "cut");
    let (all, _) = frames(dir.path());
    let said = all.last().unwrap();
    assert_eq!((said.0, said.2), (at, KIND_CHECKPOINT));
    let step = |what: &str, at: u64, len: u64| (what.to_owned(), at, len);
    let expected = [
        step("pwrite", at, said.1 - at),
        step("fsync", 0, 0),
        step("truncate", said.1, 0),
        step("fsync", 0, 0),
    ];
    assert_eq!(calls, expected);
    assert_eq!(file_len(dir.path()), said.1);
}
