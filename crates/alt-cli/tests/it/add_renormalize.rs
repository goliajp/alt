//! `alt add --renormalize` stores tracked files again as the attributes
//! now in force clean them, which a plain `add` does not do for a file
//! whose stat did not change. As `git add --renormalize`.

use std::path::Path;
use std::process::{Command, Output};

fn alt(repo: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_alt"))
        .current_dir(repo)
        .env("ALT_NO_DAEMON", "1")
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .args(args)
        .output()
        .unwrap()
}

fn git(repo: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .env("GIT_COMMITTER_NAME", "tester")
        .env("GIT_COMMITTER_EMAIL", "t@e")
        .args(args)
        .output()
        .unwrap()
}

fn ok(o: Output) -> String {
    assert!(
        o.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout).unwrap()
}

fn write(root: &Path, rel: &str, body: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

/// Files committed with CRLF endings, then a rule that makes them text.
fn rules_change_after_the_commit(root: &Path, run: &dyn Fn(&[&str]) -> Output) {
    write(root, "crlf.txt", "a\r\nb\r\n");
    write(root, "d/crlf2.txt", "x\r\ny\r\n");
    write(root, "lf.txt", "lf\n");
    write(root, "gone.txt", "gone\n");
    ok(run(&["add", "."]));
    ok(run(&["commit", "-m", "first"]));
    write(root, ".gitattributes", "* text=auto\n");
    write(root, "lf.txt", "lf2\n");
    write(root, "untracked.txt", "new\n");
}

/// A pair of repositories taken through the same steps.
fn pair() -> (alt_os::temp::TempDir, alt_os::temp::TempDir) {
    let (a, g) = (
        alt_os::temp::tempdir().unwrap(),
        alt_os::temp::tempdir().unwrap(),
    );
    ok(alt(a.path(), &["init", "."]));
    ok(git(g.path(), &["init", "-q", "-b", "main"]));
    rules_change_after_the_commit(a.path(), &|args| alt(a.path(), args));
    rules_change_after_the_commit(g.path(), &|args| git(g.path(), args));
    (a, g)
}

/// The tree each side gets from committing its index.
fn committed_trees(a: &Path, g: &Path) -> (String, String) {
    ok(alt(a, &["commit", "-m", "next", "--allow-empty"]));
    ok(git(g, &["commit", "-q", "-m", "next", "--allow-empty"]));
    let tree = |cat: String| cat.lines().next().unwrap().to_owned();
    (
        tree(ok(alt(a, &["cat-file", "-p", "main"]))),
        tree(ok(git(g, &["cat-file", "-p", "HEAD"]))),
    )
}

#[test]
fn a_plain_add_leaves_files_staged_before_the_rule_as_they_are() {
    let (a, g) = pair();
    let (a, g) = (a.path(), g.path());
    ok(alt(a, &["add", "."]));
    ok(git(g, &["add", "."]));
    let (alt_tree, git_tree) = committed_trees(a, g);
    assert_eq!(alt_tree, git_tree);
    let stored = ok(alt(a, &["show", "main:crlf.txt"]));
    assert_eq!(stored, "a\r\nb\r\n");
}

#[test]
fn renormalizing_stores_tracked_files_again_like_git() {
    for spec in [".", "d", "crlf.txt"] {
        let (a, g) = pair();
        let (a, g) = (a.path(), g.path());
        ok(alt(a, &["add", "--renormalize", spec]));
        ok(git(g, &["add", "--renormalize", spec]));
        let st = ok(alt(a, &["status"]));
        // untracked files stay untracked, also the rules file itself
        assert!(
            st.contains("Untracked files:\n\t.gitattributes\n\tuntracked.txt\n"),
            "{spec}: {st}"
        );
        let whole = spec == ".";
        assert_eq!(
            st.contains("Changes not staged for commit:\n\tmodified:   lf.txt"),
            !whole,
            "{spec}: {st}"
        );
        let (alt_tree, git_tree) = committed_trees(a, g);
        assert_eq!(alt_tree, git_tree, "{spec}");
        let at = if spec == "d" {
            "main:d/crlf2.txt"
        } else {
            "main:crlf.txt"
        };
        let stored = ok(alt(a, &["show", at]));
        assert!(!stored.contains('\r'), "{spec}: {stored:?}");
        // the working tree still has its line endings and reads as clean
        let after = ok(alt(a, &["status"]));
        assert!(!after.contains("crlf"), "{spec}: {after}");
    }
}

#[test]
fn renormalizing_refuses_a_tracked_file_that_is_gone_and_stages_nothing() {
    let (a, g) = pair();
    let (a, g) = (a.path(), g.path());
    std::fs::remove_file(a.join("gone.txt")).unwrap();
    std::fs::remove_file(g.join("gone.txt")).unwrap();
    let before = ok(alt(a, &["status"]));
    for run in [
        alt(a, &["add", "--renormalize", "."]),
        git(g, &["add", "--renormalize", "."]),
    ] {
        assert!(!run.status.success());
        let said = String::from_utf8_lossy(&run.stderr);
        assert!(said.contains("unable to stat 'gone.txt'"), "{said}");
    }
    assert_eq!(ok(alt(a, &["status"])), before);
    assert_eq!(
        ok(git(g, &["status", "--porcelain"])),
        " D gone.txt\n M lf.txt\n?? .gitattributes\n?? untracked.txt\n"
    );
}

#[test]
fn renormalizing_names_what_it_is_given_as_git_does() {
    let (a, g) = pair();
    let (a, g) = (a.path(), g.path());
    let before = ok(alt(a, &["status"]));
    // an untracked file matches and is left alone; no path at all is no error
    for args in [
        &["add", "--renormalize", "untracked.txt"][..],
        &["add", "--renormalize"],
    ] {
        ok(alt(a, args));
        ok(git(g, args));
        assert_eq!(ok(alt(a, &["status"])), before, "{args:?}");
        assert_eq!(
            ok(git(g, &["status", "--porcelain"])),
            " M lf.txt\n?? .gitattributes\n?? untracked.txt\n",
            "{args:?}"
        );
    }
    // git says so on its standard error, followed by a hint
    let said = git(g, &["add", "--renormalize"]);
    let first = String::from_utf8_lossy(&said.stderr);
    let first = first.lines().next().unwrap();
    assert_eq!(ok(alt(a, &["add", "--renormalize"])), format!("{first}\n"));
    for run in [
        alt(a, &["add", "--renormalize", "nope"]),
        git(g, &["add", "--renormalize", "nope"]),
    ] {
        assert!(!run.status.success());
        let said = String::from_utf8_lossy(&run.stderr);
        assert!(
            said.contains("pathspec 'nope' did not match any files"),
            "{said}"
        );
    }
}
