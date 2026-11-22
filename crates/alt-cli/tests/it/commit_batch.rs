//! A commit stores its tree, the commit and the note on it under one
//! hold of the write lock of the object store, seen from a library
//! preloaded into the command.

#![cfg(target_os = "linux")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::staged_durability::{add, repo};
use super::unconfirmed::{alt_command, ok};

/// A library that appends to the file `ALT_TEST_LOCKS` names, for every
/// `flock` of an `odb.lock` that succeeds, the operation and how long
/// the `map.alt` beside it was just before the call.
fn library(root: &Path) -> PathBuf {
    let source = root.join("locks.c");
    let library = root.join("locks.so");
    fs::write(
        &source,
        r#"
#define _GNU_SOURCE
#include <dlfcn.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>
int flock(int fd, int op) {
    int (*real)(int, int) = dlsym(RTLD_NEXT, "flock");
    const char *log = getenv("ALT_TEST_LOCKS");
    char link[64], path[4096], map[4200], line[64];
    long long size = -1;
    ssize_t n = -1;
    if (log) {
        snprintf(link, sizeof link, "/proc/self/fd/%d", fd);
        n = readlink(link, path, sizeof path - 1);
    }
    if (n > 9 && !memcmp(path + n - 9, "/odb.lock", 9)) {
        struct stat st;
        snprintf(map, sizeof map, "%.*s/map.alt", (int)(n - 9), path);
        if (stat(map, &st) == 0) size = (long long)st.st_size;
    } else {
        log = 0;
    }
    int rc = real(fd, op);
    if (log && rc == 0) {
        int len = snprintf(line, sizeof line, "%d %lld\n", op, size);
        int out = open(log, O_WRONLY | O_APPEND | O_CREAT, 0644);
        if (out >= 0) { (void)!write(out, line, len); close(out); }
    }
    return rc;
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

const LOCK_EX: u64 = 2;
const LOCK_UN: u64 = 8;

#[test]
fn a_commit_stores_its_tree_the_commit_and_the_note_under_one_hold_of_the_write_lock() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = &tmp.path().join("repo");
    fs::create_dir(dir).unwrap();
    repo(dir);
    add(dir, "b.txt");
    let library = library(tmp.path());
    let log = tmp.path().join("locks.log");
    let map = dir.join(".alt/map.alt");
    let before = fs::metadata(&map).unwrap().len();
    let mut commit = alt_command(dir, &["commit", "-m", "second"]);
    commit.env("LD_PRELOAD", &library);
    ok(commit.env("ALT_TEST_LOCKS", &log).output().unwrap());
    let after = fs::metadata(&map).unwrap().len();
    // by how much map.alt grew under each hold of the write lock
    let mut grew = Vec::new();
    let mut held = None;
    for line in fs::read_to_string(&log).unwrap().lines() {
        let (op, len) = line.split_once(' ').unwrap();
        let (op, len): (u64, u64) = (op.parse().unwrap(), len.parse().unwrap());
        if op & LOCK_EX != 0 {
            held = Some(len);
        } else if op == LOCK_UN
            && let Some(from) = held.take()
        {
            grew.push(len - from);
        }
    }
    grew.retain(|by| *by > 0);
    // a tree, the commit, and the blob, the tree and the commit of the note
    assert_eq!(grew, [after - before], "{grew:?}");
    assert!(
        after - before >= 5 * 82,
        "map.alt grew by {}",
        after - before
    );
}
