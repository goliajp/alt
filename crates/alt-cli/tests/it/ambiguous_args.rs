//! The words of a command that takes revisions and paths are told apart
//! as git tells them: by `--` where it is written, else by what each
//! word names, and a word that names both or neither is refused.

use crate::like_git::{Pair, alt, alt_daemon, fails, git, heads, index, ok};

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

/// `main` and `side` part after the first commit. `main` is a file as
/// well as a branch, `gone` a file committed and then deleted but not
/// staged so, and `d/x` a file in a directory.
fn words() -> Pair {
    let pair = Pair::new();
    let mut time = 1_700_000_000;
    let mut commit = |pair: &Pair, message: &str| {
        time += 1;
        pair.commit_all_at(message, time);
    };
    pair.write("f", "one\n");
    pair.write("main", "a file\n");
    pair.write("d/x", "x\n");
    commit(&pair, "c1");
    pair.both(&["branch", "side"]);
    pair.write("f", "two\n");
    commit(&pair, "c2");
    pair.write("gone", "here\n");
    commit(&pair, "c3");
    pair.write("d/x", "x2\n");
    commit(&pair, "c4");
    pair.remove("gone");
    pair.write("f", "three\n");
    pair
}

/// The command fails in both with 128, and alt says what git says.
fn refused(pair: &Pair, args: &[&str]) -> String {
    pair.same_failure(args, 128)
}

const NEITHER: &str = "fatal: ambiguous argument 'nosuch': unknown revision or path not in the working tree.\n\
                       Use '--' to separate paths from revisions, like this:\n\
                       'git <command> [<revision>...] -- [<file>...]'\n";

#[test]
fn log_takes_a_word_for_what_it_names() {
    let pair = words();
    for args in [
        &["log", "--oneline", "f"][..],
        &["log", "--oneline", "d"],
        &["log", "--oneline", "side", "f"],
        &["log", "--oneline", "side..main", "f", "d"],
        &["log", "--oneline", "HEAD~1"],
        &["log", "--oneline", "main", "--"],
        &["log", "--oneline", "--", "main"],
        &["log", "--oneline", "--", "gone"],
        &["log", "--oneline", "--"],
        &["log", "--oneline", "side", "--", "nosuch"],
        &["log", "--oneline", "side..main", "--", "side"],
    ] {
        pair.same(args);
    }
    assert_eq!(pair.same(&["log", "--oneline", "f"]).lines().count(), 2);
    assert_eq!(
        pair.same(&["log", "--oneline", "--", "main"])
            .lines()
            .count(),
        1
    );
    assert_eq!(
        pair.same(&["log", "--oneline", "main", "--"])
            .lines()
            .count(),
        4
    );
    assert_eq!(refused(&pair, &["log", "main"]), AMBIGUOUS);
    assert_eq!(refused(&pair, &["log", "nosuch"]), NEITHER);
    assert_eq!(refused(&pair, &["log", "side", "nosuch"]), NEITHER);
    assert_eq!(
        refused(&pair, &["log", "nosuch", "--"]),
        "fatal: bad revision 'nosuch'\n"
    );
    assert_eq!(
        refused(&pair, &["log", "f", "side"]),
        "fatal: side: no such path in the working tree.\n\
         Use 'git <command> -- <path>...' to specify paths that do not exist locally.\n"
    );
    // a file that is gone is no path without the dashes, and a range one
    // of whose sides names nothing is no revision
    for args in [
        &["log", "gone"][..],
        &["log", "nosuch..main"],
        &["log", "nosuch..main", "--"],
        &["log", "HEAD~9"],
        &["log", "HEAD~9", "--"],
    ] {
        refused(&pair, args);
    }
}

#[test]
fn log_reads_a_path_from_where_it_runs() {
    let pair = words();
    let (a, g) = (pair.alt.join("d"), pair.git.join("d"));
    for args in [
        &["log", "--oneline", "x"][..],
        &["log", "--oneline", "../f"],
        &["log", "--oneline", "side", "x"],
    ] {
        assert_eq!(ok(alt(&a, args)), ok(git(&g, args)), "{args:?}");
    }
    // `f` is a file of the directory above
    let said = fails(alt(&a, &["log", "f"]), 128);
    let git_said = fails(git(&g, &["log", "f"]), 128);
    assert!(said.starts_with(&git_said), "alt: {said}\ngit: {git_said}");
}

#[test]
fn diff_takes_a_word_for_what_it_names() {
    let pair = words();
    for args in [
        &["diff", "--name-only", "f"][..],
        &["diff", "f"],
        &["diff", "--name-only", "d"],
        &["diff", "--name-only", "side", "f"],
        &["diff", "--name-status", "side", "HEAD", "f", "d"],
        &["diff", "--name-status", "side..HEAD", "d"],
        &["diff", "--name-only", "--cached", "f"],
        &["diff", "--name-only", "--cached", "HEAD~2", "f"],
        &["diff", "--name-only", "main", "--"],
        &["diff", "--name-only", "--", "main"],
        &["diff", "--name-status", "HEAD~1", "--", "gone"],
        &["diff", "--name-status", "side", "HEAD", "--"],
        &["diff", "--name-only", "HEAD", "--", "f", "nosuch"],
    ] {
        pair.same(args);
    }
    assert_eq!(pair.same(&["diff", "--name-only", "f"]), "f\n");
    assert_eq!(pair.same(&["diff", "--name-only", "side", "d"]), "d/x\n");
    assert_eq!(refused(&pair, &["diff", "main"]), AMBIGUOUS);
    assert_eq!(refused(&pair, &["diff", "side", "main"]), AMBIGUOUS);
    assert_eq!(refused(&pair, &["diff", "nosuch"]), NEITHER);
    assert_eq!(
        refused(&pair, &["diff", "HEAD~1...HEAD", "nosuch"]),
        NEITHER
    );
    assert_eq!(
        refused(&pair, &["diff", "nosuch", "--"]),
        "fatal: bad revision 'nosuch'\n"
    );
    refused(&pair, &["diff", "f", "side"]);
    refused(&pair, &["diff", "gone"]);
    refused(&pair, &["diff", "nosuch..main", "--"]);
}

#[test]
fn reset_takes_the_word_before_the_dashes_for_a_commit() {
    let pair = words();
    // `main` is a file as well: the dashes say the branch is meant
    pair.both(&["reset", "-q", "side"]);
    pair.assert_same_state();
    pair.same(&["reset", "main", "--"]);
    pair.assert_same_state();
    pair.both(&["add", "f"]);
    pair.same(&["reset", "HEAD", "--"]);
    pair.assert_same_state();
    pair.both(&["add", "f"]);
    pair.same(&["reset", "--hard", "HEAD~1", "--"]);
    pair.assert_same_state();
    assert_eq!(
        refused(&pair, &["reset", "nosuch", "--"]),
        "fatal: Failed to resolve 'nosuch' as a valid revision.\n"
    );
    assert_eq!(
        refused(&pair, &["reset", "nosuch", "--", "f"]),
        "fatal: Failed to resolve 'nosuch' as a valid tree.\n"
    );
    pair.assert_same_state();
}

#[test]
fn reset_takes_a_word_for_what_it_names() {
    let pair = words();
    pair.both(&["add", "-A"]);
    // only the first of the paths must be there
    pair.same(&["reset", "f", "side"]);
    pair.assert_same_state();
    pair.both(&["add", "-A"]);
    pair.same(&["reset", "side", "f"]);
    pair.assert_same_state();
    pair.both(&["add", "-A"]);
    pair.same(&["reset", "HEAD~1", "nosuch", "d"]);
    pair.assert_same_state();
    pair.same(&["reset", "f*"]);
    pair.assert_same_state();
    // a file that is gone is no path without the dashes
    assert_eq!(refused(&pair, &["reset", "nosuch"]), NEITHER);
    refused(&pair, &["reset", "gone"]);
    refused(&pair, &["reset", "HEAD~9"]);
    pair.assert_same_state();
    pair.same(&["reset", "--", "gone"]);
    pair.assert_same_state();
}

#[test]
fn checkout_takes_the_word_before_the_dashes_for_a_branch() {
    let pair = words();
    pair.both(&["checkout", "f"]);
    pair.both(&["checkout", "side", "--"]);
    pair.assert_same_state();
    pair.both(&["checkout", "main", "--"]);
    pair.assert_same_state();
    for args in [
        &["checkout", "nosuch", "--"][..],
        &["checkout", "nosuch", "--", "f"],
    ] {
        assert_eq!(refused(&pair, args), "fatal: invalid reference: nosuch\n");
    }
    // the first word a revision, the rest are paths, the file `main` too
    pair.both(&["checkout", "side", "main", "f"]);
    pair.assert_same_state();
    pair.both(&["checkout", "f", "main"]);
    pair.assert_same_state();
}

#[test]
fn the_dashes_alone_reach_the_daemon_and_the_request() {
    let (direct, served) = (words(), words());
    for args in [
        &["reset", "side"][..],
        &["reset", "main", "--"],
        &["checkout", "side", "--"],
    ] {
        assert_eq!(
            ok(alt_daemon(&served.alt, args)),
            ok(alt(&direct.alt, args))
        );
        assert_eq!(index(&served.alt), index(&direct.alt));
        assert_eq!(heads(&served.alt), heads(&direct.alt));
    }
    let bound = |line: &str| {
        let args = line.split(' ').map(std::ffi::OsString::from).collect();
        let parsed = alt_cli::machine::parse(args).unwrap_or_else(|_| panic!("{line}"));
        alt_json::to_string(&parsed.normalized()).unwrap()
    };
    assert_ne!(bound("alt reset main --"), bound("alt reset main"));
    assert!(bound("alt reset main --").contains(r#""--":[]"#));
    assert!(!bound("alt reset main -- f").contains(r#""--""#));
    assert!(!bound("alt add --").contains(r#""--""#));
}
