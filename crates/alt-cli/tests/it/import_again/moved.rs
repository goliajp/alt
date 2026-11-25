//! A second import knows the repository it met by what it is, not by
//! where it is: one that was moved is the one met before, and one made
//! anew at the path of another is told by what the export left in it.

use std::path::Path;

use crate::import_met::imported;
use crate::like_git::{alt, git, ok};

/// Commits a change of `f` in the store; the commit `main` is at.
fn commit_here(to: &Path) -> String {
    std::fs::write(to.join("f"), "made here").unwrap();
    ok(alt(to, &["add", "f"]));
    ok(alt(to, &["commit", "-m", "made here"]));
    ok(alt(to, &["rev-parse", "main"]))
}

/// Imports `source` into `to` again; `main` is left at `made`, with no
/// word of a branch put back.
fn assert_kept(source: &Path, to: &Path, made: &str) {
    let again = alt(source, &["import", to.to_str().unwrap()]);
    let said = String::from_utf8_lossy(&again.stderr).into_owned();
    assert!(again.status.success(), "{said}");
    assert!(!said.contains("warning: branch"), "{said}");
    assert_eq!(ok(alt(to, &["rev-parse", "main"])), made);
}

#[test]
fn a_repository_moved_since_it_was_imported_is_the_one_met_before() {
    let (dir, source, to) = imported();
    let made = commit_here(&to);
    let moved = dir.path().join("moved");
    std::fs::rename(&source, &moved).unwrap();
    ok(git(&moved, &["branch", "other"]));
    assert_kept(&moved, &to, &made);
    assert!(alt(&to, &["rev-parse", "other"]).status.success());
    // and again from there, with what was at the old path forgotten
    ok(git(&moved, &["branch", "third"]));
    assert_kept(&moved, &to, &made);
    let met = std::fs::read_to_string(to.join(".alt/git-import/met")).unwrap();
    assert_eq!(met.matches("git /").count(), 1, "{met}");
}

#[test]
fn a_repository_exported_anew_where_one_was_imported_from_is_told_by_what_the_export_left() {
    let (_dir, source, to) = imported();
    std::fs::remove_dir_all(&source).unwrap();
    ok(alt(&to, &["export", source.to_str().unwrap()]));
    let made = commit_here(&to);
    ok(git(&source, &["branch", "other"]));
    assert_kept(&source, &to, &made);
    assert!(alt(&to, &["rev-parse", "other"]).status.success());
}
