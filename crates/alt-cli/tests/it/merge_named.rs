//! `alt merge` of what is no branch name: a commit by its id, a tag, a
//! branch of a remote, an earlier commit of a branch, the branch left
//! last. Each beside git's, down to the id of the merge commit.

mod settings;

use std::path::{Path, PathBuf};

use crate::like_git::{alt, alt_daemon, fails, files, git, heads, index, ok};

/// A clone of a git repository and an import of that clone.
struct Cloned {
    _dir: alt_os::temp::TempDir,
    alt: PathBuf,
    git: PathBuf,
}

/// At the remote, `main` with one commit and `topic` with one more. In
/// the clone, `side` with two commits of its own, the tag `light` and
/// the annotated tag `ann` at its second, `base` at the first commit of
/// all, and `main` a commit further on and checked out after `side`.
fn cloned() -> Cloned {
    let dir = alt_os::temp::tempdir().unwrap();
    let at = |name: &str| dir.path().join(name);
    let (up, clone, imported) = (at("up"), at("clone"), at("imported"));
    std::fs::create_dir_all(&up).unwrap();
    ok(git(&up, &["init", "-q", "."]));
    let commit = |root: &Path, name: &str, content: &str, message: &str| {
        std::fs::write(root.join(name), content).unwrap();
        ok(git(root, &["add", "."]));
        ok(git(root, &["commit", "-q", "-m", message]));
    };
    commit(&up, "a", "1\n", "first");
    ok(git(&up, &["switch", "-q", "-c", "topic"]));
    commit(&up, "t", "t\n", "topic one");
    ok(git(&up, &["switch", "-q", "main"]));
    let (source, to) = (up.to_str().unwrap(), clone.to_str().unwrap());
    ok(git(dir.path(), &["clone", "-q", source, to]));
    ok(git(&clone, &["branch", "base"]));
    ok(git(&clone, &["switch", "-q", "-c", "side"]));
    commit(&clone, "s", "s\n", "side one");
    commit(&clone, "s", "s\ns2\n", "side two");
    ok(git(&clone, &["tag", "light"]));
    ok(git(&clone, &["tag", "-a", "-m", "the notes of ann", "ann"]));
    ok(git(&clone, &["switch", "-q", "main"]));
    commit(&clone, "m", "m\n", "main moves on");
    ok(alt(&clone, &["import", imported.to_str().unwrap()]));
    Cloned {
        _dir: dir,
        alt: imported,
        git: clone,
    }
}

impl Cloned {
    fn both(&self, args: &[&str]) {
        ok(alt(&self.alt, args));
        ok(git(&self.git, args));
    }

    fn id(&self, rev: &str) -> String {
        ok(git(&self.git, &["rev-parse", rev])).trim().to_owned()
    }

    fn assert_same_state(&self, told: &str) {
        assert_eq!(heads(&self.alt), heads(&self.git), "{told}");
        assert_eq!(
            index(&self.alt),
            ok(git(&self.git, &["ls-files", "-s"])),
            "{told}"
        );
        assert_eq!(files(&self.alt), files(&self.git), "{told}");
    }

    /// Merges `word` in both, with `more` before it; what git then has
    /// for the message of the commit HEAD is at.
    fn merge(&self, more: &[&str], word: &str) -> String {
        let args = [&["merge"][..], more, &[word]].concat();
        ok(alt(&self.alt, &args));
        let args = [&["merge", "--no-edit"][..], more, &[word]].concat();
        ok(git(&self.git, &args));
        self.assert_same_state(word);
        ok(git(&self.git, &["log", "-1", "--format=%B"]))
    }

    /// The last line of the log of HEAD, in both the same.
    fn same_last_move(&self, word: &str) {
        let args = ["reflog", "-n", "1"];
        assert_eq!(
            ok(alt(&self.alt, &args)),
            ok(git(&self.git, &args)),
            "{word}"
        );
    }
}

#[test]
fn a_merge_takes_any_commit_and_names_it_as_git_does() {
    let both = cloned();
    let (main, side) = (both.id("main"), both.id("side"));
    let short = &side[..9];
    for (word, begins, logged) in [
        (side.as_str(), format!("Merge commit '{side}'\n"), true),
        (short, format!("Merge commit '{short}'\n"), true),
        ("light", "Merge tag 'light'\n".to_owned(), true),
        (
            "ann",
            "Merge tag 'ann'\n\nthe notes of ann\n".to_owned(),
            true,
        ),
        (
            "tags/ann",
            "Merge tag 'tags/ann'\n\nthe notes of ann\n".to_owned(),
            true,
        ),
        ("ann^{}", "Merge commit 'ann^{}'\n".to_owned(), true),
        ("light~1", "Merge commit 'light~1'\n".to_owned(), true),
        ("side", "Merge branch 'side'\n".to_owned(), true),
        (
            "refs/heads/side",
            "Merge branch 'refs/heads/side'\n".to_owned(),
            true,
        ),
        ("heads/side", "Merge branch 'heads/side'\n".to_owned(), true),
        // git logs the word, of which the message keeps the branch alone
        (
            "side~1",
            "Merge branch 'side' (early part)\n".to_owned(),
            false,
        ),
        (
            "side^",
            "Merge branch 'side' (early part)\n".to_owned(),
            false,
        ),
        ("side~0", "Merge branch 'side'\n".to_owned(), false),
        (
            "origin/topic",
            "Merge remote-tracking branch 'origin/topic'\n".to_owned(),
            true,
        ),
        (
            "refs/remotes/origin/topic",
            "Merge remote-tracking branch 'refs/remotes/origin/topic'\n".to_owned(),
            true,
        ),
        ("-", "Merge branch 'side'\n".to_owned(), false),
        ("@{-1}", "Merge branch 'side'\n".to_owned(), false),
    ] {
        let message = both.merge(&[], word);
        assert!(message.starts_with(&begins), "{word}: {message}");
        if logged {
            both.same_last_move(word);
        }
        both.both(&["reset", "--hard", &main]);
    }
    // the branch merged into is named where it is not main
    both.both(&["switch", "-c", "dev"]);
    assert_eq!(
        both.merge(&[], "ann"),
        "Merge tag 'ann' into dev\n\nthe notes of ann\n\n"
    );
    both.both(&["reset", "--hard", &main]);
    let named = format!("Merge commit '{side}' into dev\n\n");
    assert_eq!(both.merge(&[], &side), named);
}

#[test]
fn a_commit_ahead_is_moved_to_or_merged_as_the_options_say() {
    let both = cloned();
    let (base, side) = (both.id("base"), both.id("side"));
    both.both(&["switch", "base"]);
    for word in [side.as_str(), "light", "ann", "origin/topic"] {
        both.merge(&[], word);
        assert_eq!(both.id("HEAD"), both.id(&format!("{word}^{{commit}}")));
        // a move alone leaves no message to keep the word: the log has the
        // one ref that stands at the commit, where there is just one
        if word == "origin/topic" {
            both.same_last_move(word);
        }
        both.both(&["reset", "--hard", &base]);
    }
    both.merge(&["--ff-only"], "ann");
    both.both(&["reset", "--hard", &base]);
    assert_eq!(
        both.merge(&["--no-ff"], "light"),
        "Merge tag 'light' into base\n\n"
    );
    // what is merged already is nothing to merge
    let said = ok(alt(&both.alt, &["merge", &base]));
    assert_eq!(said, ok(git(&both.git, &["merge", &base])));
    assert_eq!(said, "Already up to date.\n");
    both.assert_same_state("up to date");
}

#[test]
fn a_word_that_names_no_commit_is_refused_in_the_words_of_git() {
    let both = cloned();
    let at = heads(&both.alt);
    for (word, said) in [
        (
            "nosuch",
            "merge: nosuch - not something we can merge\n".to_owned(),
        ),
        (
            "HEAD:a",
            "error: HEAD:a: expected commit type, but the object dereferences to blob type\n\
             merge: HEAD:a - not something we can merge\n"
                .to_owned(),
        ),
        (
            "side^{tree}",
            "error: side^{tree}: expected commit type, but the object dereferences to tree type\n\
             merge: side^{tree} - not something we can merge\n"
                .to_owned(),
        ),
    ] {
        let ours = fails(alt(&both.alt, &["merge", word]), 1);
        assert!(ours.starts_with(&said), "{word}: {ours}");
        assert_eq!(fails(git(&both.git, &["merge", word]), 1), said, "{word}");
    }
    assert_eq!(heads(&both.alt), at);
}

#[test]
fn a_conflict_marks_the_side_by_the_word_it_was_asked_with() {
    let both = cloned();
    for root in [&both.alt, &both.git] {
        std::fs::write(root.join("s"), "mine\n").unwrap();
    }
    both.both(&["add", "s"]);
    ok(alt(&both.alt, &["commit", "-m", "mine"]));
    ok(git(&both.git, &["commit", "-q", "-m", "mine"]));
    let side = both.id("side");
    for word in [side.as_str(), "ann", "side~1", "origin/topic~0", "-"] {
        let ours = alt(&both.alt, &["merge", word]);
        let theirs = git(&both.git, &["merge", "--no-edit", word]);
        let conflicts = word != "origin/topic~0";
        assert_eq!(ours.status.code(), Some(i32::from(conflicts)), "{word}");
        assert_eq!(theirs.status.code(), ours.status.code(), "{word}");
        assert_eq!(files(&both.alt), files(&both.git), "{word}");
        if conflicts {
            let marked = std::fs::read_to_string(both.alt.join("s")).unwrap();
            let named = if word == "-" { "@{-1}" } else { word };
            assert!(marked.ends_with(&format!(">>>>>>> {named}\n")), "{marked}");
            both.both(&["merge", "--abort"]);
        } else {
            both.assert_same_state(word);
            both.both(&["reset", "--hard", "HEAD~1"]);
        }
    }
}

#[test]
fn the_daemon_and_a_parallel_workspace_merge_a_commit_alike_and_it_is_undone() {
    let (direct, served) = (cloned(), cloned());
    let side = direct.id("side");
    let before = (files(&direct.alt), index(&direct.alt), heads(&direct.alt));
    for args in [&["merge", "ann"][..], &["merge", "--json", &side]] {
        let (ours, theirs) = (alt_daemon(&served.alt, args), alt(&direct.alt, args));
        assert_eq!(ours.status.code(), theirs.status.code(), "{args:?}");
        assert_eq!(files(&served.alt), files(&direct.alt));
        assert_eq!(index(&served.alt), index(&direct.alt));
        assert_eq!(heads(&served.alt), heads(&direct.alt));
    }
    ok(git(&direct.git, &["merge", "--no-edit", "ann"]));
    direct.assert_same_state("ann");
    ok(alt(&direct.alt, &["undo"]));
    assert_eq!(
        (files(&direct.alt), index(&direct.alt), heads(&direct.alt)),
        before
    );

    let trees = alt_os::temp::tempdir().unwrap();
    let tree = trees.path().join("ws");
    ok(alt(
        &direct.alt,
        &["workspace", "add", "ws", tree.to_str().unwrap()],
    ));
    ok(alt(&tree, &["switch", "-c", "w1", "base"]));
    ok(alt(&tree, &["merge", "--no-ff", "ann"]));
    assert_eq!(std::fs::read_to_string(tree.join("s")).unwrap(), "s\ns2\n");
    let message = ok(alt(&direct.alt, &["log", "-n", "1", "--pretty=raw", "w1"]));
    assert!(message.contains("Merge tag 'ann' into w1"), "{message}");
    // the default workspace is where the undo left it
    assert_eq!(
        (files(&direct.alt), index(&direct.alt)),
        (before.0, before.1)
    );
}
