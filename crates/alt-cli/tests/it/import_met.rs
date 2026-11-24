//! A second import takes what git changed since the store last met the
//! repository, and leaves what the store made in the meantime: a stash
//! made here, a branch moved here, the notes of the commits made here.

use std::path::{Path, PathBuf};

use crate::like_git::{alt, export, git, ok};

/// A git repository with two commits, and the store an import of it
/// makes.
fn imported() -> (alt_os::temp::TempDir, PathBuf, PathBuf) {
    let dir = alt_os::temp::tempdir().unwrap();
    let (source, to) = (dir.path().join("source"), dir.path().join("to"));
    std::fs::create_dir(&source).unwrap();
    ok(git(&source, &["init", "-q", "."]));
    for text in ["one", "two"] {
        std::fs::write(source.join("f"), text).unwrap();
        ok(git(&source, &["add", "f"]));
        ok(git(&source, &["commit", "-q", "-m", text]));
    }
    ok(alt(&source, &["import", to.to_str().unwrap()]));
    (dir, source, to)
}

fn import(source: &Path, to: &Path) {
    ok(alt(source, &["import", to.to_str().unwrap()]));
}

/// Stashes `text` as the content of `f`, with `program`; the commit of
/// the entry.
fn stash(program: &str, repo: &Path, text: &str) -> String {
    std::fs::write(repo.join("f"), text).unwrap();
    let (stashed, top) = match program {
        "git" => (
            git(repo, &["stash"]),
            git(repo, &["rev-parse", "refs/stash"]),
        ),
        _ => (
            alt(repo, &["stash"]),
            alt(repo, &["rev-parse", "refs/stash"]),
        ),
    };
    ok(stashed);
    ok(top).trim().to_owned()
}

/// The stack of the store, the oldest entry first.
fn stack(to: &Path) -> Vec<String> {
    let mut stack = Vec::new();
    loop {
        let entry = alt(to, &["rev-parse", &format!("refs/stashes/{}", stack.len())]);
        if !entry.status.success() {
            break;
        }
        stack.push(ok(entry).trim().to_owned());
    }
    if let Some(top) = stack.last() {
        assert_eq!(ok(alt(to, &["rev-parse", "refs/stash"])).trim(), top);
        let listed = ok(alt(to, &["stash", "list"]));
        assert_eq!(listed.lines().count(), stack.len(), "{listed}");
    } else {
        assert!(!alt(to, &["rev-parse", "refs/stash"]).status.success());
    }
    stack
}

/// The stack git has, the oldest entry first.
fn git_stack(source: &Path) -> Vec<String> {
    let listed = git(source, &["reflog", "show", "--format=%H", "refs/stash"]);
    let listed = match listed.status.success() {
        true => ok(listed),
        false => String::new(),
    };
    listed.lines().rev().map(str::to_owned).collect()
}

#[test]
fn a_second_import_keeps_a_stash_made_here_and_drops_one_git_dropped() {
    let (_dir, source, to) = imported();
    let g1 = stash("git", &source, "g1");
    import(&source, &to);
    assert_eq!(stack(&to), std::slice::from_ref(&g1));
    // made here, and not exported: git never had it
    let a1 = stash("alt", &to, "a1");
    import(&source, &to);
    assert_eq!(stack(&to), [g1.clone(), a1.clone()]);
    // git drops the one it had and makes another
    ok(git(&source, &["stash", "drop", "-q"]));
    let g2 = stash("git", &source, "g2");
    import(&source, &to);
    assert_eq!(stack(&to), [g2.clone(), a1.clone()]);
    // exported, git has the one made here as well, and may drop it
    let out = export(&to);
    let source = out.path().join("exported");
    assert_eq!(git_stack(&source), [g2.clone(), a1.clone()]);
    import(&source, &to);
    assert_eq!(stack(&to), [g2.clone(), a1]);
    ok(git(&source, &["stash", "drop", "-q"]));
    import(&source, &to);
    assert_eq!(stack(&to), [g2]);
    ok(git(&source, &["stash", "drop", "-q"]));
    import(&source, &to);
    assert!(stack(&to).is_empty());
}

#[test]
fn a_stash_stays_where_nothing_tells_whether_git_dropped_it() {
    let (_dir, source, to) = imported();
    let g1 = stash("git", &source, "g1");
    import(&source, &to);
    let a1 = stash("alt", &to, "a1");
    // as a store an older build imported into has it
    std::fs::remove_file(to.join(".alt/git-import/met")).unwrap();
    ok(git(&source, &["stash", "drop", "-q"]));
    import(&source, &to);
    assert_eq!(stack(&to), [g1, a1]);
}

#[test]
fn a_second_import_leaves_the_refs_git_did_not_change() {
    let (_dir, source, to) = imported();
    let at = |rev: &str| ok(alt(&to, &["rev-parse", rev]));
    let commit = |text: &str| {
        std::fs::write(to.join("f"), text).unwrap();
        ok(alt(&to, &["add", "f"]));
        ok(alt(&to, &["commit", "-m", text]));
    };
    let theirs = at("main");
    commit("a");
    ok(alt(&to, &["branch", "made-here"]));
    ok(alt(&to, &["tag", "tagged-here"]));
    ok(alt(&to, &["switch", "-c", "on-here"]));
    let state = |to: &Path| {
        let refs = [
            "HEAD",
            "main",
            "made-here",
            "tagged-here",
            "refs/notes/alt/meta",
        ];
        let named = refs.map(|name| ok(alt(to, &["rev-parse", name])));
        (
            named,
            ok(alt(to, &["status"])),
            ok(alt(to, &["reflog", "main"])),
        )
    };
    let before = state(&to);
    assert_ne!(before.0[1], theirs);
    // git changed nothing: neither does the import
    import(&source, &to);
    assert_eq!(state(&to), before);
    // git moves the branch: the store has it where git has it, and the
    // log of the branch still leads to what was made here
    std::fs::write(source.join("f"), "three").unwrap();
    ok(git(&source, &["commit", "-q", "-a", "-m", "three"]));
    let moved = ok(git(&source, &["rev-parse", "main"]));
    import(&source, &to);
    assert_eq!(at("main"), moved);
    assert_eq!(at("main@{1}"), before.0[1]);
    let log = ok(alt(&to, &["reflog", "main"]));
    let first = log.lines().next().unwrap_or_default();
    assert!(first.contains("main@{0}: import: moving to "), "{log}");
    for kept in ["made-here", "tagged-here", "refs/notes/alt/meta"] {
        assert!(alt(&to, &["rev-parse", kept]).status.success(), "{kept}");
    }
    assert!(ok(alt(&to, &["status"])).contains("On branch on-here"));
    // exported, the repository has what the store has: what is made
    // here after that stands again
    let out = export(&to);
    let source = out.path().join("exported");
    commit("b");
    let before = state(&to);
    import(&source, &to);
    assert_eq!(state(&to), before);
}
