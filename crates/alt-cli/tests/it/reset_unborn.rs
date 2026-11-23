//! `alt reset <commit>` on a branch that has no commit yet makes the
//! branch, as git does.

use crate::like_git::{alt, git, heads, ok};

#[test]
fn a_reset_on_a_branch_without_a_commit_puts_the_branch_at_the_commit() {
    for mode in ["--hard", "--mixed", "--soft"] {
        let dir = alt_os::temp::tempdir().unwrap();
        let (source, imported) = (dir.path().join("git"), dir.path().join("alt"));
        std::fs::create_dir(&source).unwrap();
        ok(git(&source, &["init", "-q", "."]));
        std::fs::write(source.join("a"), "a\n").unwrap();
        ok(git(&source, &["add", "a"]));
        ok(git(&source, &["commit", "-q", "-m", "one"]));
        ok(git(&source, &["switch", "-q", "--orphan", "fresh"]));
        ok(alt(&source, &["import", imported.to_str().unwrap()]));

        ok(git(&source, &["reset", "-q", mode, "main"]));
        ok(alt(&imported, &["reset", "-q", mode, "main"]));
        assert_eq!(heads(&imported), heads(&source), "{mode}");
        let tip = ok(alt(&imported, &["rev-parse", "fresh"]));
        assert_eq!(tip, ok(git(&source, &["rev-parse", "main"])), "{mode}");
        // the next commit has the commit reset to as its parent
        std::fs::write(imported.join("b"), "b\n").unwrap();
        ok(alt(&imported, &["add", "-A"]));
        ok(alt(&imported, &["commit", "-m", "two"]));
        assert_eq!(ok(alt(&imported, &["rev-parse", "HEAD~1"])), tip, "{mode}");
    }
}
