//! `alt cherry-pick --continue` and `--abort`: a pick that stopped,
//! committed or given up, checked against git doing the same: run
//! directly, through the daemon, and by git.

use crate::cwd_paths::{Trees, git};
use crate::scan_daemon::output;
use crate::stopped_trees::{
    TREES, alike, commit, diverged, every, head, operation, succeeded, working_tree,
};

/// `cat-file -p` of `rev` in `tree`.
pub(crate) fn shown(trees: &Trees, tree: &str, rev: &str) -> String {
    let at = trees.at(tree);
    let said = match tree {
        "git" => git(&at, &["cat-file", "-p", rev]),
        _ => output(&at, &["cat-file", "-p", rev], false),
    };
    assert!(said.status.success(), "{tree}: cat-file -p {rev}");
    String::from_utf8(said.stdout).unwrap()
}

/// Who a header of a commit names and when, without the time zone.
pub(crate) fn header(commit: &str, name: &str) -> (String, String) {
    let line = commit.lines().find(|l| l.starts_with(name)).unwrap();
    let (who, rest) = line.split_once("> ").unwrap();
    (who.to_owned(), rest.split(' ').next().unwrap().to_owned())
}

/// The message of a commit, each commit id in it replaced.
fn message(commit: &str) -> String {
    let words = commit.split_once("\n\n").unwrap().1.split(' ');
    let masked = words.map(|word| {
        let id = word.trim_end_matches([')', '\n']);
        match id.len() == 40 && id.bytes().all(|b| b.is_ascii_hexdigit()) {
            true => word.replacen(id, "<id>", 1),
            false => word.to_owned(),
        }
    });
    masked.collect::<Vec<_>>().join(" ")
}

/// The trees of [`diverged`], whose commit on `side` has another author
/// than the one that picks it.
pub(crate) fn by_another_author() -> Trees {
    let trees = diverged();
    assert_eq!(every(&trees, &["switch", "side"]), [Some(0); 3]);
    trees.write("f.txt", "1\nside by another\n3\n");
    assert_eq!(every(&trees, &["add", "."]), [Some(0); 3]);
    for tree in TREES {
        let at = trees.at(tree);
        let mut commit = match tree {
            "git" => std::process::Command::new("git"),
            _ => crate::alt_command(),
        };
        commit
            .current_dir(&at)
            .env("ALT_NO_DAEMON", "1")
            .env("XDG_CONFIG_HOME", at.join(".alt/test-config"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "another")
            .env("GIT_AUTHOR_EMAIL", "a@e")
            .env("GIT_COMMITTER_NAME", "another")
            .env("GIT_COMMITTER_EMAIL", "a@e")
            .args(["commit", "-m", "side again\n\nwith a body"]);
        assert!(commit.output().unwrap().status.success(), "{tree}");
    }
    assert_eq!(every(&trees, &["switch", "main"]), [Some(0); 3]);
    trees
}

/// Stops a pick of the last commit of `side` in each tree. git is asked
/// for the line naming the commit picked, which alt always writes.
pub(crate) fn stop(trees: &Trees) {
    for (tree, daemon) in [("direct", false), ("served", true)] {
        let picked = output(&trees.at(tree), &["cherry-pick", "side"], daemon);
        assert_eq!(picked.status.code(), Some(1), "{tree}");
        // where it stops it names both ways out
        let said = String::from_utf8_lossy(&picked.stdout).into_owned();
        let ways = "run `alt cherry-pick --continue` (or `alt cherry-pick --abort`)";
        assert!(said.contains(ways), "{tree}: {said}");
    }
    let picked = git(&trees.at("git"), &["cherry-pick", "-x", "side"]);
    assert_eq!(picked.status.code(), Some(1));
    assert_eq!(operation(trees), r#"{"kind":"cherry-pick"}"#);
}

/// git commits a pick through its editor, which is told to change nothing.
fn go_on(trees: &Trees) -> [bool; 3] {
    let ours = |tree: &str, daemon: bool| {
        let done = output(&trees.at(tree), &["cherry-pick", "--continue"], daemon);
        done.status.success()
    };
    let theirs = std::process::Command::new("git")
        .current_dir(trees.at("git"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_EDITOR", "true")
        .env("GIT_COMMITTER_NAME", "tester")
        .env("GIT_COMMITTER_EMAIL", "t@e")
        .args(["cherry-pick", "--continue"])
        .output()
        .unwrap();
    [
        ours("direct", false),
        ours("served", true),
        theirs.status.success(),
    ]
}

#[test]
fn continue_commits_the_resolved_pick_as_git_does() {
    let trees = by_another_author();
    let before: Vec<_> = TREES.iter().map(|tree| head(&trees, tree)).collect();
    stop(&trees);
    trees.write("f.txt", "1\nresolved\n3\n");
    assert_eq!(every(&trees, &["add", "f.txt"]), [Some(0); 3]);
    // a change that is not staged is no part of the commit and stays
    trees.write("u.txt", "not staged\n");
    assert_eq!(go_on(&trees), [true; 3]);

    alike(&trees, "after the pick");
    assert_eq!(operation(&trees), "null");
    let theirs = shown(&trees, "git", "HEAD");
    for (at, tree) in TREES.iter().enumerate() {
        let (commit, picked) = (shown(&trees, tree, "HEAD"), shown(&trees, tree, "side"));
        let parents: Vec<&str> = commit
            .lines()
            .filter(|l| l.starts_with("parent "))
            .collect();
        assert_eq!(parents, [format!("parent {}", before[at].trim())], "{tree}");
        // the author of the commit picked, with the time it was written
        assert_eq!(header(&commit, "author "), header(&picked, "author "));
        assert_eq!(header(&commit, "author ").0, "author another <a@e");
        assert_eq!(header(&commit, "committer ").0, "committer tester <t@e");
        assert_eq!(message(&commit), message(&theirs), "{tree}");
        assert_eq!(commit.lines().next(), theirs.lines().next(), "{tree}");
    }
    assert_eq!(
        message(&theirs),
        "side again\n\nwith a body\n\n(cherry picked from commit <id>)\n"
    );
    assert_eq!(working_tree(&trees, "direct")["u.txt"].0, b"not staged\n");
    for tree in ["direct", "served"] {
        assert!(!trees.at(tree).join(".alt/CHERRY_PICK_HEAD").exists());
    }

    // the pick is over
    assert_eq!(go_on(&trees), [false; 3]);
    assert_eq!(succeeded(&trees, &["cherry-pick", "--abort"]), [false; 3]);
}

#[test]
fn continue_is_refused_while_a_conflict_is_left_or_nothing_is_staged() {
    let trees = by_another_author();
    let before: Vec<_> = TREES.iter().map(|tree| head(&trees, tree)).collect();
    stop(&trees);
    assert_eq!(go_on(&trees), [false; 3]);
    let said = output(&trees.at("direct"), &["cherry-pick", "--continue"], false);
    let said = String::from_utf8_lossy(&said.stderr).into_owned();
    assert!(
        said.contains("cherry-pick --continue: resolve the conflicts and `alt add` them first"),
        "{said}"
    );
    assert_eq!(operation(&trees), r#"{"kind":"cherry-pick"}"#);

    // resolved to what the branch has: nothing is left of the commit
    trees.write("f.txt", "1\nmain\n3\n");
    assert_eq!(every(&trees, &["add", "f.txt"]), [Some(0); 3]);
    assert_eq!(go_on(&trees), [false; 3]);
    assert_eq!(operation(&trees), r#"{"kind":"cherry-pick"}"#);
    let after: Vec<_> = TREES.iter().map(|tree| head(&trees, tree)).collect();
    assert_eq!(after, before);

    assert_eq!(every(&trees, &["cherry-pick", "--abort"]), [Some(0); 3]);
    alike(&trees, "after the abort");
}

#[test]
fn abort_puts_the_index_and_the_files_back_as_git_does() {
    let trees = diverged();
    let before: Vec<_> = TREES.iter().map(|tree| head(&trees, tree)).collect();
    let files = working_tree(&trees, "direct");
    assert_eq!(every(&trees, &["cherry-pick", "side"]), [Some(1); 3]);
    assert_eq!(operation(&trees), r#"{"kind":"cherry-pick"}"#);
    trees.write("u.txt", "changed since\n");
    trees.write("untracked.txt", "mine\n");
    trees.write("f.txt", "half resolved\n");

    assert_eq!(every(&trees, &["cherry-pick", "--abort"]), [Some(0); 3]);
    alike(&trees, "after the abort");
    let after: Vec<_> = TREES.iter().map(|tree| head(&trees, tree)).collect();
    assert_eq!(after, before);
    assert_eq!(operation(&trees), "null");
    let mut kept = files;
    kept.insert("u.txt".into(), (b"changed since\n".to_vec(), false));
    kept.insert("untracked.txt".into(), (b"mine\n".to_vec(), false));
    for tree in ["direct", "served"] {
        assert_eq!(working_tree(&trees, tree), kept, "{tree}");
        assert!(!trees.at(tree).join(".alt/CHERRY_PICK_HEAD").exists());
    }

    // nothing is left of the pick: the commit can be picked again
    for file in ["u.txt", "untracked.txt"] {
        trees.write(file, &format!("{file}\n"));
    }
    commit(&trees, "in between");
    assert_eq!(every(&trees, &["cherry-pick", "side"]), [Some(1); 3]);
}

#[test]
fn abort_is_refused_where_it_would_write_over_what_is_not_staged() {
    let trees = diverged();
    assert_eq!(every(&trees, &["cherry-pick", "side"]), [Some(1); 3]);
    trees.write("t.txt", "mine\n");
    assert_eq!(succeeded(&trees, &["cherry-pick", "--abort"]), [false; 3]);
    assert_eq!(operation(&trees), r#"{"kind":"cherry-pick"}"#);
    for tree in TREES {
        assert_eq!(working_tree(&trees, tree)["t.txt"].0, b"mine\n", "{tree}");
    }
    let said = output(&trees.at("direct"), &["cherry-pick", "--abort"], false);
    let said = String::from_utf8_lossy(&said.stderr).into_owned();
    assert!(
        said.contains("cherry-pick --abort: 't.txt' has changes"),
        "{said}"
    );

    // an untracked file where one the pick deleted comes back
    trees.write("t.txt", "t.txt by side\n");
    trees.write("d.txt", "mine\n");
    assert_eq!(succeeded(&trees, &["cherry-pick", "--abort"]), [false; 3]);
    assert_eq!(working_tree(&trees, "served")["d.txt"].0, b"mine\n");
    assert_eq!(operation(&trees), r#"{"kind":"cherry-pick"}"#);
}

#[test]
fn without_a_pick_both_are_refused_as_those_of_a_revert_are() {
    let trees = diverged();
    for flag in ["--continue", "--abort"] {
        assert_eq!(succeeded(&trees, &["cherry-pick", flag]), [false; 3]);
        for (tree, daemon) in [("direct", false), ("served", true)] {
            let pick = output(&trees.at(tree), &["cherry-pick", flag], daemon);
            let revert = output(&trees.at(tree), &["revert", flag], daemon);
            assert_eq!(pick.status.code(), revert.status.code(), "{tree} {flag}");
            let said = String::from_utf8_lossy(&pick.stderr).into_owned();
            let wanted = format!("cherry-pick {flag}: no cherry-pick in progress");
            assert!(said.contains(&wanted), "{said}");
        }
    }
    // a commit by hand concludes a pick: there is none to go on with
    assert_eq!(every(&trees, &["cherry-pick", "side"]), [Some(1); 3]);
    trees.write("f.txt", "by hand\n");
    trees.write("both.txt", "by hand\n");
    commit(&trees, "picked by hand");
    for flag in ["--continue", "--abort"] {
        assert_eq!(succeeded(&trees, &["cherry-pick", flag]), [false; 3]);
    }
    alike(&trees, "after the refusals");
    // a commit and a flag together are no command, in git as in alt
    for flag in ["--continue", "--abort"] {
        let both = ["cherry-pick", flag, "side"];
        assert_eq!(succeeded(&trees, &both), [false; 3], "{flag}");
    }
}

#[test]
fn undo_takes_the_commit_of_a_continue_back_and_an_abort_too() {
    let trees = by_another_author();
    let at = trees.at("direct");
    let before = head(&trees, "direct");
    assert_eq!(
        output(&at, &["cherry-pick", "side"], false).status.code(),
        Some(1)
    );
    std::fs::write(at.join("f.txt"), "1\nresolved\n3\n").unwrap();
    assert!(output(&at, &["add", "f.txt"], false).status.success());
    assert!(
        output(&at, &["cherry-pick", "--continue"], false)
            .status
            .success()
    );
    assert_ne!(head(&trees, "direct"), before);
    assert!(output(&at, &["undo"], false).status.success());
    assert_eq!(head(&trees, "direct"), before);
    assert_eq!(
        std::fs::read(at.join("f.txt")).unwrap(),
        b"1\nresolved\n3\n"
    );

    let at = trees.at("served");
    assert_eq!(
        output(&at, &["cherry-pick", "side"], false).status.code(),
        Some(1)
    );
    let stopped = working_tree(&trees, "served");
    assert!(
        output(&at, &["cherry-pick", "--abort"], false)
            .status
            .success()
    );
    assert_ne!(working_tree(&trees, "served"), stopped);
    assert!(output(&at, &["undo"], false).status.success());
    assert_eq!(working_tree(&trees, "served"), stopped);
}
