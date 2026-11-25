//! A revert the sequence refused, for a change that is not staged in a
//! file it writes, waits as git has it wait: skipped, the rest is made
//! without it; given up, the branch goes back to where the sequence
//! began, though the user committed in between.

use crate::like_git::{Pair, alt, fails, git, heads, index, ok};

/// `A`, `B` and `C` change a file each, `D` the file of `C` again:
/// reverting `C`, `B` and `A` stops on `C`.
fn stopped() -> Pair {
    let pair = Pair::new();
    for file in ["f", "other", "third"] {
        pair.write(file, "base\n");
    }
    pair.commit_all("base");
    for (file, text) in [("f", "A"), ("other", "B"), ("third", "C"), ("third", "D")] {
        pair.write(file, &format!("{text}\n"));
        pair.commit_all(text);
    }
    let start = ["revert", "--no-edit", "HEAD~1", "HEAD~2", "HEAD~3"];
    let (ours, theirs) = pair.run(&start);
    assert_eq!(
        (ours.status.code(), theirs.status.code()),
        (Some(1), Some(1))
    );
    pair.write("third", "resolved\n");
    pair.both(&["add", "third"]);
    pair
}

fn back_in(pair: &Pair, entry: &str) -> (std::process::Output, std::process::Output) {
    let ours = alt(&pair.alt, &["revert", entry]);
    (
        ours,
        git(&pair.git, &["-c", "core.editor=true", "revert", entry]),
    )
}

fn refused(pair: &Pair) {
    pair.write("other", "mine\n");
    let (ours, theirs) = back_in(pair, "--continue");
    let (ours, theirs) = (fails(ours, 128), fails(theirs, 128));
    assert!(ours.starts_with(&theirs), "alt: {ours}\ngit: {theirs}");
}

fn same(pair: &Pair, when: &str) {
    let theirs = ok(git(&pair.git, &["ls-files", "-s"]));
    assert_eq!(index(&pair.alt), theirs, "{when}");
    assert_eq!(heads(&pair.alt), heads(&pair.git), "{when}");
    for file in ["f", "other", "third"] {
        let held = |root: &std::path::Path| std::fs::read_to_string(root.join(file)).unwrap();
        assert_eq!(held(&pair.alt), held(&pair.git), "{when}: {file}");
    }
    let waits = pair.alt.join(".alt/REVERT_STATE").exists();
    assert_eq!(waits, pair.git.join(".git/sequencer").exists(), "{when}");
}

/// The subjects git has from the tip down; the branches are the same
/// in both, so they are those of alt.
fn subjects(pair: &Pair) -> String {
    ok(git(&pair.git, &["log", "--format=%s"]))
}

#[test]
fn a_revert_that_was_refused_is_skipped_and_the_rest_is_made() {
    let pair = stopped();
    refused(&pair);
    same(&pair, "refused");
    let (ours, theirs) = back_in(&pair, "--skip");
    ok(theirs);
    ok(ours);
    same(&pair, "skipped");
    let ours = subjects(&pair);
    assert!(
        ours.starts_with("Revert \"A\"\nRevert \"C\"\nD\n"),
        "{ours}"
    );
    assert_eq!(std::fs::read(pair.alt.join("other")).unwrap(), b"mine\n");
}

#[test]
fn a_revert_refused_after_a_commit_of_the_user_is_given_up_from_where_it_began() {
    let pair = stopped();
    pair.both(&["commit", "-m", "own"]);
    refused(&pair);
    same(&pair, "refused");
    let (ours, theirs) = back_in(&pair, "--abort");
    ok(theirs);
    ok(ours);
    same(&pair, "given up");
    let ours = subjects(&pair);
    assert!(ours.starts_with("D\nC\n"), "{ours}");
    assert_eq!(std::fs::read(pair.alt.join("other")).unwrap(), b"mine\n");
    // moved by the user after the refusal, the branch is theirs again
    let pair = stopped();
    refused(&pair);
    pair.both(&["restore", "other"]);
    pair.write("extra", "new\n");
    pair.commit_all("unrelated");
    let (ours, theirs) = back_in(&pair, "--skip");
    let (ours, theirs) = (fails(ours, 128), fails(theirs, 128));
    // git's words, with the command to try under its name here
    let theirs = theirs.replace("\"git revert", "\"alt revert");
    assert!(ours.starts_with(&theirs), "alt: {ours}\ngit: {theirs}");
    let (ours, theirs) = back_in(&pair, "--abort");
    ok(theirs);
    ok(ours);
    same(&pair, "given up after a commit");
}
