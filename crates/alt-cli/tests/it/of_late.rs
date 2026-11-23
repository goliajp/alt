//! Where git changed its words or its status on the way to 2.56. alt goes
//! by git 2.56: beside a git that new the two are held to the same, and
//! beside an older one alt is held to what 2.56 does and that git only to
//! failing where 2.56 fails.

use std::path::Path;
use std::process::Output;

use crate::like_git::{Pair, alt, fails, git, git_version};

fn late() -> bool {
    git_version() >= (2, 56)
}

/// `ours` ended with `code`, as `theirs` did or, older, would by now.
pub fn same_status(ours: &Output, theirs: &Output, code: i32, told: &str) {
    assert_eq!(ours.status.code(), Some(code), "{told}");
    match late() {
        true => assert_eq!(theirs.status.code(), Some(code), "{told}"),
        false => assert_eq!(theirs.status.success(), code == 0, "{told}"),
    }
}

/// The command fails in both: alt with `code`, saying `said` before its
/// line about the request, which is all git 2.56 says.
pub fn same_failure(pair: &Pair, args: &[&str], code: i32, said: &str) {
    same_failure_at((&pair.alt, &pair.git), args, code, said);
}

/// [`same_failure`] of the command run in the directory `ours` by alt
/// and in `theirs` by git.
pub fn same_failure_at((ours, theirs): (&Path, &Path), args: &[&str], code: i32, said: &str) {
    let ours = fails(alt(ours, args), code);
    assert!(ours.starts_with(said), "{args:?}: {ours}");
    let theirs = git(theirs, args);
    match late() {
        true => assert_eq!(fails(theirs, code), said, "{args:?}"),
        false => assert!(!theirs.status.success(), "{args:?}"),
    }
}
