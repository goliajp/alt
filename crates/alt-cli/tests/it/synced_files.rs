//! Which files a command fsyncs, seen from a library preloaded into it
//! and into the daemon it starts.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A library that appends the path of every file given to `fsync` or
/// `fdatasync` to the file `ALT_TEST_SYNCED` names, then makes the call.
pub(super) fn library(root: &Path) -> PathBuf {
    let source = root.join("synced-files.c");
    let library = root.join("synced-files.so");
    fs::write(
        &source,
        r#"
#define _GNU_SOURCE
#include <dlfcn.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
static void note(int fd) {
    const char *log = getenv("ALT_TEST_SYNCED");
    char link[64], path[4096];
    if (!log) return;
    snprintf(link, sizeof link, "/proc/self/fd/%d", fd);
    ssize_t n = readlink(link, path, sizeof path - 1);
    if (n < 0) return;
    path[n++] = '\n';
    int out = open(log, O_WRONLY | O_APPEND | O_CREAT, 0644);
    if (out >= 0) { (void)!write(out, path, n); close(out); }
}
int fsync(int fd) { note(fd); int (*real)(int) = dlsym(RTLD_NEXT, "fsync"); return real(fd); }
int fdatasync(int fd) { note(fd); int (*real)(int) = dlsym(RTLD_NEXT, "fdatasync"); return real(fd); }
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

/// Runs `command` with `library` preloaded and returns what it answered
/// with the files it synced, in order, as paths beneath `dir`; a file
/// that was renamed after it was synced is named as it was then.
pub(super) fn synced(dir: &Path, library: &Path, mut command: Command) -> (Output, Vec<String>) {
    let log = library.with_extension("log");
    let _ = fs::remove_file(&log);
    let out = command
        .env("LD_PRELOAD", library)
        .env("ALT_TEST_SYNCED", &log)
        .output()
        .unwrap();
    let dir = fs::canonicalize(dir).unwrap();
    let paths = fs::read_to_string(&log).unwrap_or_default();
    let beneath = |path: &str| match Path::new(path).strip_prefix(&dir) {
        Ok(rest) => rest.to_string_lossy().into_owned(),
        Err(_) => path.to_owned(),
    };
    (out, paths.lines().map(beneath).collect())
}
