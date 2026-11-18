//! `alt diff` against git on trees that hold submodules and renames.

use std::path::Path;

use super::diff::{alt, git, hunk_body, ok};

/// A git repository whose only commit holds `a.txt` and a submodule entry
/// at `sub`, imported into a fresh alt work tree.
fn imported_with_submodule(src: &Path, dst: &Path) {
    ok(git(src, &["init", "-q", "-b", "main", "."]));
    ok(git(src, &["config", "user.email", "t@e"]));
    ok(git(src, &["config", "user.name", "t"]));
    std::fs::write(src.join("a.txt"), "one\ntwo\n").unwrap();
    ok(git(src, &["add", "a.txt"]));
    ok(git(
        src,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            "160000,1111111111111111111111111111111111111111,sub",
        ],
    ));
    ok(git(src, &["commit", "-qm", "base"]));
    ok(alt(src, &["import", dst.to_str().unwrap()]));
}

#[test]
fn diff_skips_an_untouched_submodule_entry() {
    let (src, dst) = (
        alt_os::temp::tempdir().unwrap(),
        alt_os::temp::tempdir().unwrap(),
    );
    imported_with_submodule(src.path(), dst.path());
    let root = dst.path();
    assert!(root.join("sub").is_dir(), "placeholder directory");
    assert!(ok(alt(root, &["diff"])).is_empty(), "clean tree => no diff");

    std::fs::write(root.join("a.txt"), "one\ntwo\nthree\n").unwrap();
    let out = ok(alt(root, &["diff"]));
    assert!(out.contains("diff --git a/a.txt b/a.txt"), "{out}");
    assert!(out.contains("+three"), "{out}");
    assert!(!out.contains("sub"), "{out}");

    // without the placeholder the entry is deleted, as for git
    std::fs::remove_dir(root.join("sub")).unwrap();
    std::fs::write(root.join("a.txt"), "one\ntwo\n").unwrap();
    let ours = ok(alt(root, &["diff"]));
    let theirs = ok(git(src.path(), &["-c", "core.pager=cat", "diff"]));
    assert!(theirs.contains("deleted file mode 160000"), "{theirs}");
    assert_eq!(ours, theirs);
    assert!(ok(alt(root, &["status"])).contains("deleted:    sub"));
}

#[test]
fn diff_shows_a_submodule_replaced_by_a_file_as_git_does() {
    let (src, dst) = (
        alt_os::temp::tempdir().unwrap(),
        alt_os::temp::tempdir().unwrap(),
    );
    imported_with_submodule(src.path(), dst.path());
    for root in [src.path(), dst.path()] {
        let _ = std::fs::remove_dir(root.join("sub"));
        std::fs::write(root.join("sub"), "now a file\n").unwrap();
    }
    let ours = ok(alt(dst.path(), &["diff"]));
    let theirs = ok(git(src.path(), &["-c", "core.pager=cat", "diff"]));
    assert!(ours.contains("-Subproject commit 1111111111111111111111111111111111111111"));
    assert_eq!(hunk_body(&ours), hunk_body(&theirs));
}

#[test]
fn diff_cached_pairs_a_renamed_file_as_git_does() {
    let (src, dst) = (
        alt_os::temp::tempdir().unwrap(),
        alt_os::temp::tempdir().unwrap(),
    );
    let (src, dst) = (src.path(), dst.path());
    ok(git(src, &["init", "-q", "-b", "main", "."]));
    ok(git(src, &["config", "user.email", "t@e"]));
    ok(git(src, &["config", "user.name", "t"]));
    let body: String = (1..=12).map(|n| format!("line {n}\n")).collect();
    std::fs::write(src.join("old name.txt"), &body).unwrap();
    std::fs::write(src.join("plain.txt"), "stays\n").unwrap();
    std::fs::write(src.join("gone.txt"), "nothing like the others\n").unwrap();
    ok(git(src, &["add", "."]));
    ok(git(src, &["commit", "-qm", "base"]));
    ok(alt(src, &["import", dst.to_str().unwrap()]));

    for root in [src, dst] {
        std::fs::remove_file(root.join("old name.txt")).unwrap();
        std::fs::create_dir(root.join("sub")).unwrap();
        std::fs::write(
            root.join("sub/new.txt"),
            body.replace("line 7\n", "seven\n"),
        )
        .unwrap();
        std::fs::rename(root.join("plain.txt"), root.join("sub/plain.txt")).unwrap();
        std::fs::remove_file(root.join("gone.txt")).unwrap();
        std::fs::write(root.join("fresh.txt"), "new content\n").unwrap();
    }
    ok(alt(dst, &["add", "."]));
    ok(git(src, &["add", "-A"]));
    let ours = ok(alt(dst, &["diff", "--cached"]));
    assert_eq!(
        ours,
        ok(git(src, &["-c", "core.pager=cat", "diff", "--cached"]))
    );
    assert!(
        ours.contains("rename from old name.txt\nrename to sub/new.txt\n"),
        "{ours}"
    );
    assert!(
        ours.contains("similarity index 100%\nrename from plain.txt\n"),
        "{ours}"
    );
    assert!(ours.contains("deleted file mode 100644"), "{ours}");
}
