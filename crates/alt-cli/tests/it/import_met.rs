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
    // the branch that leaves the commits made here is named, with
    // where it was and how they are kept
    let told = alt(&source, &["import", to.to_str().unwrap()]);
    let told = String::from_utf8_lossy(&told.stderr).into_owned();
    let was = format!("warning: branch 'main' was at {}", before.0[1].trim());
    assert!(told.contains(&was), "{told}");
    let keep = format!("`alt branch <name> {}`", before.0[1].trim());
    assert!(
        told.contains(&keep) && told.contains("`alt reflog main`"),
        "{told}"
    );
    assert!(!told.contains("made-here"), "{told}");
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

/// What the store has `main`, `side`, the tag `v1` and HEAD at.
fn named(to: &Path) -> Vec<String> {
    let refs = ["HEAD", "main", "side", "v1"];
    let mut named: Vec<String> = refs
        .iter()
        .map(|r| ok(alt(to, &["rev-parse", r])))
        .collect();
    named.push(ok(alt(to, &["status"])));
    named
}

#[test]
fn a_store_made_anew_where_one_was_takes_every_ref_of_what_the_old_one_exported() {
    let (_dir, _source, to) = imported();
    ok(alt(&to, &["branch", "side"]));
    ok(alt(&to, &["tag", "v1"]));
    let before = named(&to);
    let out = export(&to);
    let exported = out.path().join("exported");
    // the store is gone, with its files; what it exported is imported
    // to where it was
    std::fs::remove_dir_all(to.join(".alt")).unwrap();
    std::fs::remove_file(to.join("f")).unwrap();
    let told = ok(alt(&exported, &["import", to.to_str().unwrap()]));
    assert!(!told.contains("already up to date"), "{told}");
    assert_eq!(named(&to), before);
    // and a branch the store deleted comes back where git still has it
    ok(alt(&to, &["branch", "-d", "side"]));
    import(&exported, &to);
    assert_eq!(named(&to), before);
}

#[test]
fn a_store_brought_back_as_it_once_was_takes_what_it_exported_since() {
    let (dir, _source, to) = imported();
    let kept = dir.path().join("kept");
    let copy = |from: &Path, to: &Path| {
        let copied = std::process::Command::new("cp")
            .arg("-R")
            .args([from, to])
            .status();
        assert!(copied.unwrap().success());
    };
    copy(&to.join(".alt"), &kept);
    std::fs::write(to.join("f"), "later").unwrap();
    ok(alt(&to, &["add", "f"]));
    ok(alt(&to, &["commit", "-m", "later"]));
    let later = ok(alt(&to, &["rev-parse", "main"]));
    let out = export(&to);
    let exported = out.path().join("exported");
    std::fs::remove_dir_all(to.join(".alt")).unwrap();
    copy(&kept, &to.join(".alt"));
    assert_ne!(ok(alt(&to, &["rev-parse", "main"])), later);
    import(&exported, &to);
    assert_eq!(ok(alt(&to, &["rev-parse", "main"])), later);
}

#[test]
fn a_repository_cloned_anew_where_one_was_has_dropped_no_stash() {
    let (dir, source, to) = imported();
    let entry = stash("git", &source, "put away");
    import(&source, &to);
    assert_eq!(stack(&to), std::slice::from_ref(&entry));
    // another repository at the path of the first, with no stash
    let again = dir.path().join("again");
    ok(git(&source, &["clone", "-q", ".", again.to_str().unwrap()]));
    std::fs::remove_dir_all(&source).unwrap();
    std::fs::rename(&again, &source).unwrap();
    assert!(git_stack(&source).is_empty());
    import(&source, &to);
    assert_eq!(stack(&to), [entry]);
}

#[test]
fn what_an_export_left_is_named_where_it_cannot_be_read() {
    let (_dir, _source, to) = imported();
    let out = export(&to);
    let exported = out.path().join("exported");
    let left = std::fs::read_to_string(exported.join(".git/alt-met")).unwrap();
    let head: Vec<&str> = left.lines().take(2).collect();
    assert!(head[1].ends_with(" :store"), "{left}");
    let broken = format!("{}\n{}\nno ref at all\n", head[0], head[1]);
    std::fs::write(exported.join(".git/alt-met"), broken).unwrap();
    let refused = alt(&exported, &["import", to.to_str().unwrap()]);
    assert!(!refused.status.success());
    let said = String::from_utf8_lossy(&refused.stderr).into_owned();
    assert!(
        said.contains("alt-met") && !said.contains("git-import/met"),
        "{said}"
    );
}

#[test]
fn a_branch_git_renamed_to_beneath_its_own_name_or_back_is_imported() {
    let (_dir, source, to) = imported();
    let at = |rev: &str| ok(alt(&to, &["rev-parse", rev]));
    let logged = |branch: &str| ok(alt(&to, &["reflog", branch])).lines().count();
    ok(git(&source, &["branch", "topic"]));
    import(&source, &to);
    let lines = logged("topic");
    assert!(lines > 0);
    // the name the store has a branch at is a directory of refs in git
    ok(git(&source, &["branch", "-m", "topic", "topic/a"]));
    import(&source, &to);
    assert_eq!(at("topic/a"), ok(git(&source, &["rev-parse", "topic/a"])));
    assert_eq!(at("topic"), at("topic/a"));
    assert_eq!(logged("topic"), lines);
    let beneath = logged("topic/a");
    assert!(beneath > 0);
    // and back: the branch of the store lies beneath a ref of git
    ok(git(&source, &["branch", "-m", "topic/a", "topic"]));
    import(&source, &to);
    assert_eq!(at("topic"), ok(git(&source, &["rev-parse", "topic"])));
    assert_eq!(logged("topic/a"), beneath);
    assert!(logged("topic") > 0);
    import(&source, &to);
    assert_eq!(logged("topic/a"), beneath);
}
