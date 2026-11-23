//! A word before `--` that names both a revision and a file is refused
//! as git refuses it, by every command that takes either there.

use crate::like_git::Pair;

/// A repository whose branch `main` has a file called `main`.
fn both_named() -> Pair {
    let pair = Pair::new();
    pair.write("f", "one\n");
    pair.write("main", "a file\n");
    pair.commit_all("one");
    pair.write("f", "two\n");
    pair.commit_all("two");
    pair
}

const AMBIGUOUS: &str = "fatal: ambiguous argument 'main': both revision and filename\n\
                         Use '--' to separate paths from revisions, like this:\n\
                         'git <command> [<revision>...] -- [<file>...]'\n";

#[test]
fn reset_and_checkout_refuse_a_word_that_is_a_revision_and_a_file() {
    let pair = both_named();
    pair.write("f", "three\n");
    pair.both(&["add", "f"]);
    for args in [&["reset", "main"][..], &["checkout", "main", "f"]] {
        assert_eq!(pair.same_failure(args, 128), AMBIGUOUS, "{args:?}");
        pair.assert_same_state();
    }
    // `--` says a path is meant, the full name of the branch that it is
    pair.both(&["reset", "-q", "--", "main"]);
    pair.both(&["reset", "-q", "refs/heads/main"]);
    pair.assert_same_state();
}
