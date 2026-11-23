//! The stash through git and back: an export has the stack as git keeps
//! it and nothing else, and an import reads the stack git has.

use std::path::Path;

use crate::like_git::{alt, export, git, ok};

fn stack(repo: &Path) -> String {
    ok(git(repo, &["stash", "list", "--format=%H %gs"]))
}

/// The entries of an alt repository, each with its commit and subject.
fn alt_stack(repo: &Path) -> String {
    let listed = ok(alt(repo, &["stash", "list", "--json"]));
    let mut lines = String::new();
    for entry in listed.split("\"commit\":\"").skip(1) {
        let commit = entry.split('"').next().unwrap();
        let subject = entry.split("\"message\":\"").nth(1).unwrap();
        lines.push_str(&format!(
            "{commit} {}\n",
            subject.split('"').next().unwrap()
        ));
    }
    lines
}

/// A git repository with a commit and two stashed entries, and the
/// directory it is imported into.
fn stashed() -> (
    alt_os::temp::TempDir,
    std::path::PathBuf,
    std::path::PathBuf,
) {
    let dir = alt_os::temp::tempdir().unwrap();
    let (source, imported) = (dir.path().join("git"), dir.path().join("alt"));
    std::fs::create_dir(&source).unwrap();
    ok(git(&source, &["init", "-q", "."]));
    std::fs::write(source.join("a"), "a\n").unwrap();
    ok(git(&source, &["add", "a"]));
    ok(git(&source, &["commit", "-q", "-m", "one"]));
    for text in ["first\n", "second\n"] {
        std::fs::write(source.join("a"), text).unwrap();
        ok(git(&source, &["stash", "-q"]));
    }
    ok(alt(&source, &["import", imported.to_str().unwrap()]));
    (dir, source, imported)
}

#[test]
fn an_export_has_the_refs_the_git_repository_had() {
    let (_dir, source, imported) = stashed();
    let out = export(&imported);
    let exported = out.path().join("exported");
    let refs = |repo: &Path| ok(git(repo, &["for-each-ref"]));
    assert_eq!(refs(&exported), refs(&source));
    assert_eq!(stack(&exported), stack(&source));
    assert_eq!(alt_stack(&imported), stack(&source));
}

#[test]
fn an_import_reads_the_stack_as_git_left_it_after_an_export() {
    let (_dir, _, imported) = stashed();
    let out = export(&imported);
    let exported = out.path().join("exported");
    ok(git(&exported, &["checkout", "-q", "main", "--", "."]));
    ok(git(&exported, &["stash", "drop", "-q", "stash@{1}"]));
    std::fs::write(exported.join("a"), "third\n").unwrap();
    ok(git(&exported, &["stash", "-q"]));
    assert_eq!(stack(&exported).lines().count(), 2);
    ok(alt(&exported, &["import", imported.to_str().unwrap()]));
    assert_eq!(alt_stack(&imported), stack(&exported));
}

#[test]
fn an_import_empties_the_stack_git_emptied() {
    let (_dir, source, imported) = stashed();
    assert_eq!(alt_stack(&imported).lines().count(), 2);
    ok(git(&source, &["stash", "clear"]));
    ok(alt(&source, &["import", imported.to_str().unwrap()]));
    assert_eq!(alt_stack(&imported), "");
    assert!(
        !alt(&imported, &["rev-parse", "refs/stash"])
            .status
            .success()
    );
    // and one entry less is one entry less
    std::fs::write(source.join("a"), "again\n").unwrap();
    ok(git(&source, &["stash", "-q"]));
    ok(alt(&source, &["import", imported.to_str().unwrap()]));
    assert_eq!(alt_stack(&imported), stack(&source));
}
