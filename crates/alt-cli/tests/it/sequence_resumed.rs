//! A revert, a rebase, a cherry-pick and a merge that stopped, gone on
//! with or given up: a change that is not staged is written over where
//! git writes over it and nowhere else, and a branch the user moved
//! since is not taken back.

use std::process::{Command, Output};

use crate::like_git::{Pair, SIGNING, alt, fails, files, heads, index, ok};

/// The command in the alt repository alone.
fn alone(pair: &Pair, args: &[&str]) -> Output {
    crate::alt_command()
        .current_dir(&pair.alt)
        .env("ALT_NO_DAEMON", "1")
        .env("ALT_PROTECTED_OVERRIDE", "1")
        .env("GIT_EDITOR", "true")
        .envs(SIGNING)
        .args(args)
        .output()
        .unwrap()
}

/// The same command in both, with an editor that changes nothing.
fn run(pair: &Pair, args: &[&str]) -> (Output, Output) {
    let ours = alone(pair, args);
    let theirs = Command::new("git")
        .current_dir(&pair.git)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_EDITOR", "true")
        .env("TZ", "UTC")
        .envs(SIGNING)
        .args(args)
        .output()
        .unwrap();
    (ours, theirs)
}

fn both(pair: &Pair, args: &[&str]) {
    let (ours, theirs) = run(pair, args);
    ok(ours);
    ok(theirs);
}

/// Both stop: on a conflict, or refusing.
fn stops(pair: &Pair, args: &[&str]) {
    let (ours, theirs) = run(pair, args);
    assert!(
        !ours.status.success() && !theirs.status.success(),
        "{args:?}"
    );
}

/// Both refuse with `code`, and alt says what git says; what git said.
fn refuse(pair: &Pair, args: &[&str], code: i32) -> String {
    let (ours, theirs) = run(pair, args);
    let (said, git_said) = (fails(ours, code), fails(theirs, code));
    assert!(said.starts_with(&git_said), "alt: {said}\ngit: {git_said}");
    git_said
}

/// The files and the index are the same in both.
fn same_tree(pair: &Pair) {
    assert_eq!(
        index(&pair.alt),
        ok(crate::like_git::git(&pair.git, &["ls-files", "-s"]))
    );
    assert_eq!(files(&pair.alt), files(&pair.git));
}

fn same(pair: &Pair) {
    same_tree(pair);
    assert_eq!(heads(&pair.alt), heads(&pair.git));
}

/// `main` with four commits: a base, `A` and `C` that change the first
/// line of `f` each, and between them `B` that changes `other`. `third`
/// is changed by none.
fn history() -> Pair {
    let pair = Pair::new();
    pair.write("f", "l1\nl2\nl3\n");
    pair.write("other", "base\n");
    pair.write("third", "t\n");
    pair.commit_all("base");
    pair.write("f", "A1\nl2\nl3\n");
    pair.commit_all("A");
    pair.write("other", "B\n");
    pair.commit_all("B");
    pair.write("f", "X1\nl2\nl3\n");
    pair.commit_all("C");
    pair
}

/// [`history`], and `side` from the base with its own first line of
/// `f`: `A` and `C` each stop on it.
fn with_side() -> Pair {
    let pair = history();
    both(&pair, &["switch", "-c", "side", "HEAD~3"]);
    pair.write("f", "S1\nl2\nl3\n");
    pair.commit_all("S");
    pair
}

fn resolve(pair: &Pair) {
    pair.write("f", "resolved\nl2\nl3\n");
    both(pair, &["add", "f"]);
}

#[test]
fn a_revert_gone_on_with_writes_over_no_change_that_is_not_staged() {
    // the revert that waits writes the file that was changed
    let pair = history();
    stops(&pair, &["revert", "--no-edit", "HEAD~2", "HEAD~1"]);
    resolve(&pair);
    pair.write("other", "mine\n");
    let said = refuse(&pair, &["revert", "--continue"], 128);
    assert!(
        said.contains("would be overwritten by merge:\n\tother\n"),
        "{said}"
    );
    // the one that was resolved is committed, and the other waits
    same(&pair);
    // asked to go on once more, git ends the sequence without the
    // revert it refused and says nothing; alt makes it
    ok(alt(&pair.alt, &["restore", "other"]));
    ok(crate::like_git::git(&pair.git, &["checkout", "other"]));
    both(&pair, &["revert", "--continue"]);
    let other = |dir: &std::path::Path| std::fs::read_to_string(dir.join("other")).unwrap();
    assert_eq!(
        (other(&pair.alt), other(&pair.git)),
        ("base\n".into(), "B\n".into())
    );
    let said = ok(alt(&pair.alt, &["log", "-n", "1"]));
    assert!(said.contains("Revert \"B\""), "{said}");
    assert!(!pair.alt.join(".alt/REVERT_STATE").exists());
    let said = ok(alt(&pair.alt, &["status"]));
    assert!(
        said.contains("nothing to commit, working tree clean"),
        "{said}"
    );
    // it does not write the file that was changed
    let pair = history();
    stops(&pair, &["revert", "--no-edit", "HEAD~2", "HEAD~1"]);
    resolve(&pair);
    pair.write("third", "mine\n");
    both(&pair, &["revert", "--continue"]);
    same(&pair);
    // concluded by a commit, the rest goes on the same
    let pair = history();
    stops(&pair, &["revert", "--no-edit", "HEAD~2", "HEAD~1"]);
    resolve(&pair);
    both(&pair, &["commit", "-m", "own"]);
    pair.write("other", "mine\n");
    refuse(&pair, &["revert", "--continue"], 128);
    same(&pair);
}

#[test]
fn a_revert_given_up_takes_back_no_branch_the_user_moved() {
    let moved = "warning: You seem to have moved HEAD. Not rewinding, check your HEAD!\n";
    for further in [false, true] {
        let pair = history();
        stops(&pair, &["revert", "--no-edit", "HEAD~2", "HEAD~1"]);
        resolve(&pair);
        both(&pair, &["commit", "-m", "own"]);
        if further {
            pair.write("extra", "new\n");
            pair.commit_all("unrelated");
        }
        let (ours, theirs) = run(&pair, &["revert", "--abort"]);
        let said = String::from_utf8_lossy(&ours.stderr).into_owned();
        assert_eq!(ok(theirs), "");
        ok(ours);
        assert!(said.starts_with(moved), "{said}");
        same(&pair);
        // and nothing is left to go on with
        stops(&pair, &["revert", "--continue"]);
        same(&pair);
    }
    // the commits the revert made itself are taken back
    let pair = history();
    stops(&pair, &["revert", "--no-edit", "HEAD~1", "HEAD~2"]);
    same_tree(&pair);
    pair.write("third", "mine\n");
    both(&pair, &["revert", "--abort"]);
    same(&pair);
    assert_eq!(
        std::fs::read_to_string(pair.alt.join("third")).unwrap(),
        "mine\n"
    );
}

#[test]
fn a_rebase_gone_on_with_or_left_treats_what_is_not_staged_as_git_does() {
    let unstaged = "You must edit all merge conflicts and then\n\
                    mark them as resolved using git add\n";
    // gone on with: nothing is committed while a change is not staged,
    // whichever file it is in
    for changed in ["other", "third"] {
        let pair = with_side();
        both(&pair, &["switch", "main"]);
        stops(&pair, &["rebase", "side"]);
        same_tree(&pair);
        resolve(&pair);
        pair.write(changed, "mine\n");
        let (ours, theirs) = run(&pair, &["rebase", "--continue"]);
        assert_eq!(
            (ours.status.code(), theirs.status.code()),
            (Some(1), Some(1))
        );
        let said = |o: &Output| String::from_utf8_lossy(&o.stdout).into_owned();
        assert_eq!(
            (said(&ours).as_str(), said(&theirs).as_str()),
            (unstaged, unstaged)
        );
        same_tree(&pair);
        both(&pair, &["restore", changed]);
        stops(&pair, &["rebase", "--continue"]);
        same_tree(&pair);
        pair.write("f", "again\nl2\nl3\n");
        both(&pair, &["add", "f"]);
        both(&pair, &["rebase", "--continue"]);
        same(&pair);
    }
    // skipped: git resets hard and the change is lost; here the skip is
    // refused, and made once the change is taken back
    for changed in ["other", "third"] {
        let pair = with_side();
        both(&pair, &["switch", "main"]);
        stops(&pair, &["rebase", "side"]);
        pair.write(changed, "mine\n");
        let before = index(&pair.alt);
        let (ours, theirs) = run(&pair, &["rebase", "--skip"]);
        assert_eq!(theirs.status.code(), Some(1), "{changed}");
        let said = fails(ours, 1);
        assert!(
            said.contains(&format!("'{changed}' has changes that are not staged")),
            "{said}"
        );
        assert_eq!(
            std::fs::read_to_string(pair.alt.join(changed)).unwrap(),
            "mine\n"
        );
        assert_eq!(index(&pair.alt), before);
        ok(alt(&pair.alt, &["restore", changed]));
        let skipped = alone(&pair, &["rebase", "--skip"]);
        assert_eq!(skipped.status.code(), Some(1), "{changed}");
        same_tree(&pair);
    }
    // given up: a hard reset, as by git, and an undo brings the change
    // back with the rebase
    let pair = with_side();
    both(&pair, &["switch", "main"]);
    stops(&pair, &["rebase", "side"]);
    pair.write("third", "mine\n");
    both(&pair, &["rebase", "--abort"]);
    same(&pair);
    ok(alt(&pair.alt, &["undo"]));
    assert_eq!(
        std::fs::read_to_string(pair.alt.join("third")).unwrap(),
        "mine\n"
    );
    assert!(pair.alt.join(".alt/REBASE_STATE").exists());
    ok(alt(&pair.alt, &["rebase", "--abort"]));
    same(&pair);
    // where the file is written the change goes, as with git, and an
    // undo of the abort has it again
    let pair = with_side();
    both(&pair, &["switch", "main"]);
    stops(&pair, &["rebase", "side"]);
    pair.write("other", "mine\n");
    both(&pair, &["rebase", "--abort"]);
    same(&pair);
    ok(alt(&pair.alt, &["undo"]));
    assert_eq!(
        std::fs::read_to_string(pair.alt.join("other")).unwrap(),
        "mine\n"
    );
}

#[test]
fn a_cherry_pick_gone_on_with_or_given_up_keeps_what_is_not_staged_elsewhere() {
    let pair = with_side();
    stops(&pair, &["cherry-pick", "main~2"]);
    same_tree(&pair);
    resolve(&pair);
    pair.write("third", "mine\n");
    both(&pair, &["cherry-pick", "--continue"]);
    same(&pair);
    let pair = with_side();
    stops(&pair, &["cherry-pick", "main~2"]);
    pair.write("third", "mine\n");
    both(&pair, &["cherry-pick", "--abort"]);
    same(&pair);
    // concluded by a commit, there is none to give up
    let pair = with_side();
    stops(&pair, &["cherry-pick", "main~2"]);
    resolve(&pair);
    both(&pair, &["commit", "-m", "own"]);
    stops(&pair, &["cherry-pick", "--abort"]);
    same(&pair);
}

#[test]
fn a_merge_concluded_or_given_up_keeps_what_is_not_staged_elsewhere() {
    let pair = with_side();
    stops(&pair, &["merge", "main"]);
    pair.write("third", "mine\n");
    both(&pair, &["merge", "--abort"]);
    same(&pair);
    let pair = with_side();
    stops(&pair, &["merge", "main"]);
    resolve(&pair);
    pair.write("third", "mine\n");
    both(&pair, &["commit", "-m", "merged"]);
    same(&pair);
    stops(&pair, &["merge", "--abort"]);
    same(&pair);
}
