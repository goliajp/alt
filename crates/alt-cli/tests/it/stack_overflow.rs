//! The alt binary is entered through the C `main`, where the standard
//! library installs no handler for a stack overflow: the binary sets up
//! its own, and an overflow of its main thread is told as the standard
//! runtime tells it instead of ending in a bare segmentation fault.
#![cfg(target_os = "linux")]

use std::fs;
use std::os::unix::process::ExitStatusExt;
use std::process::Command;

use super::unconfirmed::{alt, ok};

/// `alt status` in `dir`, with the stack limit `ulimit` sets when given.
fn status(dir: &std::path::Path, ulimit: &str) -> std::process::Output {
    Command::new("sh")
        .arg("-c")
        .arg(format!("{ulimit} exec \"$ALT\" status"))
        .env("ALT", env!("CARGO_BIN_EXE_alt"))
        .env("ALT_NO_DAEMON", "1")
        .current_dir(dir)
        .output()
        .unwrap()
}

#[test]
fn an_overflow_of_the_main_thread_is_reported() {
    let tmp = alt_os::temp::tempdir().unwrap();
    ok(alt(tmp.path(), &["init"]));
    // the scan of the working tree takes a stack frame for each level
    let mut deep = tmp.path().to_owned();
    for _ in 0..800 {
        deep.push("d");
    }
    fs::create_dir_all(&deep).unwrap();
    fs::write(deep.join("f.txt"), "x\n").unwrap();
    // the tree is not too deep for the stack a command usually has
    let usual = status(tmp.path(), "");
    assert!(usual.status.success(), "{usual:?}");

    let small = status(tmp.path(), "ulimit -s 128;");
    let said = String::from_utf8_lossy(&small.stderr);
    assert!(
        said.contains("thread 'main' has overflowed its stack"),
        "{said}"
    );
    // aborted, as the standard runtime does
    assert_eq!(small.status.signal(), Some(6), "{small:?}");
}
