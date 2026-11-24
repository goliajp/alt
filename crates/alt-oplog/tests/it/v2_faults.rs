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
        "sync" => sync_fails(&dir),
        "sink" => sink_sync_fails(&dir),
        "bound" => boundary_sync_fails(&dir),
        "cut-fails" => cut_sync_fails(&dir),
        other => panic!("no scenario {other}"),
    }
}

/// In boot 1: three synced ops, then `gone` and one more, of a writer
/// that does not sync, left unsynced. Returns where `gone` starts.
fn two_to_cut(dir: &Path) -> u64 {
    boot(1);
    let mut log = new_log(dir);
    grow(&mut log, "a", 3);
    log.append("a", 4, b"gone").unwrap();
    let at = log.anchor().unwrap().start;
    let _unsynced = alt_os::durability::leave_unsynced(true);
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
    // the header of the log tells of the cut first, and is synced
    let expected = [
        step("pwrite", 44, 12),
        step("fsync", 0, 0),
        step("pwrite", at, said.1 - at),
        step("fsync", 0, 0),
        step("truncate", said.1, 0),
        step("fsync", 0, 0),
    ];
    assert_eq!(calls, expected);
    assert_eq!(file_len(dir.path()), said.1);
}

/// Makes the fsyncs of the log fail from now on, or work again.
fn fail_syncs(dir: &Path, fail: bool) {
    match fail {
        true => fs::write(dir.join("fail"), b"").unwrap(),
        false => fs::remove_file(dir.join("fail")).unwrap(),
    }
}

fn io_error<T: std::fmt::Debug>(did: Result<T, OpLogError>) {
    assert!(matches!(did, Err(OpLogError::Io(_))), "{did:?}");
}

/// A sync that fails moves no boundary: not what is noted, not what the
/// handle takes for synced, not what the next frame says.
fn sync_fails(dir: &Path) {
    let mut log = new_log(dir);
    grow(&mut log, "a", 2);
    let synced = log.appended_len();
    let noted = fs::read(dir.join("tail")).unwrap();
    log.append("a", 3, b"not synced").unwrap();
    fail_syncs(dir, true);
    io_error(log.sync());
    io_error(log.bound_now());
    assert_eq!(fs::read(dir.join("tail")).unwrap(), noted);
    assert_eq!(
        (log.synced_here(), log.bounded_to()),
        (synced, Some(synced))
    );
    assert!(!log.confirmed());
    log.append("a", 4, b"after it").unwrap();
    assert_eq!(frames(dir).1.synced, synced);
    fail_syncs(dir, false);
    log.sync().unwrap();
    assert_eq!(alt_oplog::bounded_in(dir), Some(log.appended_len()));
}

/// The same of a sync left to a sink.
fn sink_sync_fails(dir: &Path) {
    let mut log = new_log(dir);
    grow(&mut log, "a", 2);
    let synced = log.appended_len();
    let noted = fs::read(dir.join("tail")).unwrap();
    log.set_defer_durability(true);
    let sink = log.sync_handle().unwrap();
    log.append("a", 3, b"not synced").unwrap();
    log.sync().unwrap();
    fail_syncs(dir, true);
    io_error(sink.fsync());
    io_error(sink.fsync());
    assert_eq!(fs::read(dir.join("tail")).unwrap(), noted);
    assert_eq!(
        (log.synced_here(), log.bounded_to()),
        (synced, Some(synced))
    );
    fail_syncs(dir, false);
    sink.fsync().unwrap();
    assert_eq!(log.synced_here(), log.appended_len());
}

/// A writer whose sync of what another boot left fails appends nothing
/// after it and sets no boundary over it.
fn boundary_sync_fails(dir: &Path) {
    boot(1);
    let mut log = new_log(dir);
    grow(&mut log, "a", 2);
    let synced = log.appended_len();
    // a ref transaction: an op a sync sees to
    let linked = super::v2_image::linked();
    log.append_with("a", 3, b"left by boot 1", &linked).unwrap();
    let len = log.appended_len();
    drop(log);
    boot(2);
    let mut log = OpLog::open(dir).unwrap();
    log.sync_named_with(std::sync::Arc::new(|_| Ok(())));
    fail_syncs(dir, true);
    io_error(log.append("b", 4, b"after it"));
    assert_eq!((file_len(dir), log.len()), (len, 3));
    assert_eq!(alt_oplog::bounded_in(dir), None);
    assert_eq!(log.bounded_to(), Some(synced));
    assert!(OpLog::open(dir).unwrap().unsettled());
    fail_syncs(dir, false);
    log.append("b", 4, b"after it").unwrap();
    assert_eq!(alt_oplog::bounded_in(dir), Some(len));
}

/// A cut whose first sync fails has told of itself in the header of the
/// log and cut nothing: the frames are all there, the log ends for a
/// reader where the cut was begun, and the next writer cuts it.
fn cut_sync_fails(dir: &Path) {
    let at = two_to_cut(dir);
    let before = fs::read(dir.join("log")).unwrap();
    boot(2);
    let mut log = OpLog::open(dir).unwrap();
    fail_syncs(dir, true);
    io_error(log.settle(but_gone));
    assert!(fs::read(dir.join("log")).unwrap()[64..] == before[64..]);
    fail_syncs(dir, false);
    let mut next = OpLog::open(dir).unwrap();
    assert_eq!((next.len(), next.state().unwrap().generation), (3, 0));
    assert!(next.passed_over());
    assert_eq!(next.settle(but_gone).unwrap().unwrap().ops, 2);
    next.append("b", 9, b"next").unwrap();
    next.sync().unwrap();
    assert_eq!(next.state().unwrap().generation, 1);
    let (all, state) = frames(dir);
    let said = all.iter().find(|frame| frame.0 == at).unwrap();
    assert_eq!(said.3.unwrap().ops, 2, "what was cut is told once");
    assert_eq!((state.seq, file_len(dir)), (4, next.appended_len()));
}

#[test]
fn a_sync_that_fails_moves_no_boundary() {
    for scenario in ["sync", "sink", "bound", "cut-fails"] {
        let dir = alt_os::temp::tempdir().unwrap();
        let calls = in_child(dir.path(), scenario);
        let failed = calls.iter().filter(|call| call.0 == "fsync-failed");
        assert!(failed.count() >= 1, "{scenario}: no sync of the log failed");
    }
}
