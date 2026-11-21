//! Points a debug build can be made to die at, as a process that is killed
//! does: for the tests of what a command cut short leaves. A release build
//! holds none of this.

use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};

static PASSED: AtomicUsize = AtomicUsize::new(0);

/// Passes the point `name`. With `ALT_CRASH_POINT=<n>` the process dies at
/// the n-th point it passes; with `ALT_CRASH_POINT=list:<file>` it writes
/// the name of each to the file and goes on.
pub(crate) fn point(name: &str) {
    let Some(asked) = std::env::var_os("ALT_CRASH_POINT") else {
        return;
    };
    let asked = asked.to_string_lossy();
    let passed = PASSED.fetch_add(1, Ordering::Relaxed) + 1;
    match asked.strip_prefix("list:") {
        Some(file) => {
            let mut list = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(file);
            let listed = list.as_mut().map(|list| writeln!(list, "{name}"));
            assert!(matches!(listed, Ok(Ok(()))), "cannot list crash points");
        }
        None if asked.parse() == Ok(passed) => std::process::abort(),
        None => {}
    }
}
