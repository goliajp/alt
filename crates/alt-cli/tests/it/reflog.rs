//! The log of a ref beside git's: `alt reflog`, the revisions that ask
//! it (`HEAD@{1}`, `main@{2}`, `@{1}`, `@{-1}`) and `switch -`.

use std::path::Path;

use crate::like_git::{Pair, alt, alt_daemon, export, fails, git, ok};
use crate::of_late;

const T: u64 = 1_700_000_000;

fn id(pair: &Pair, rev: &str) -> String {
    ok(git(&pair.git, &["rev-parse", rev])).trim().to_owned()
}

/// `main` with two commits and `side` with one more, `main` checked out.
fn branches() -> Pair {
    let pair = Pair::new();
    pair.write("a", "1\n");
    pair.commit_all_at("base", T);
    pair.write("a", "2\n");
    pair.commit_all_at("second", T + 1);
    let second = id(&pair, "HEAD");
    pair.both(&["switch", "-c", "side", &second]);
    pair.write("s", "s\n");
    pair.commit_all_at("side one", T + 2);
    pair.both(&["switch", "main"]);
    pair
}

/// [`branches`] and then a merge with a commit, a branch made and left
/// and come back to, a merge that only moves the branch on, a reset, an
/// amended commit and a revert.
fn moved() -> Pair {
    let pair = branches();
    let second = id(&pair, "main");
    pair.merge_at("side", T + 3);
    pair.both(&["switch", "-c", "other", &second]);
    pair.write("o", "o\n");
    pair.commit_all_at("other one", T + 4);
    pair.both(&["switch", "-"]);
    pair.both(&["switch", "-"]);
    pair.both(&["checkout", "-"]);
    let merged = id(&pair, "main");
    pair.both(&["switch", "-c", "on", &merged]);
    pair.write("n", "n\n");
    pair.commit_all_at("on one", T + 5);
    pair.both(&["switch", "main"]);
    pair.both(&["merge", "on"]);
    pair.both(&["reset", "--hard", &second]);
    pair.write("x", "x\n");
    pair.commit_all_at("x", T + 6);
    pair.both(&["commit", "--amend", "-m", "amended"]);
    pair.both(&["revert", "--no-edit", "HEAD"]);
    pair
}

#[test]
fn the_log_of_a_ref_is_the_one_git_keeps_of_the_same_commands() {
    let pair = moved();
    let whole = pair.same(&["reflog"]);
    assert!(
        whole.contains("HEAD@{0}: revert: Revert \"amended\"\n"),
        "{whole}"
    );
    assert!(
        whole.contains(": checkout: moving from other to main\n"),
        "{whole}"
    );
    assert!(whole.contains(": commit (initial): base\n"), "{whole}");
    assert_eq!(pair.same(&["reflog", "show"]), whole);
    assert_eq!(pair.same(&["reflog", "show", "HEAD"]), whole);
    for name in ["main", "other", "side", "on", "refs/heads/side"] {
        pair.same(&["reflog", name]);
        pair.same(&["reflog", "show", name]);
    }
    let latest = pair.same(&["reflog", "-n", "3"]);
    assert_eq!(latest.lines().count(), 3);
    assert!(whole.starts_with(&latest));
    pair.same(&["reflog", "-n", "2", "main"]);

    // a branch deleted and made again has a log that begins anew
    ok(alt(&pair.alt, &["branch", "-d", "on"]));
    ok(git(&pair.git, &["branch", "-q", "-D", "on"]));
    pair.both(&["branch", "on", &id(&pair, "side")]);
    assert_eq!(pair.same(&["reflog", "on"]).lines().count(), 1);
}

/// What each line of a log says after the id it begins with.
fn told(log: &str) -> Vec<&str> {
    let after = |line| str::split_once(line, ' ').map(|(_, said)| said);
    log.lines().filter_map(after).collect()
}

#[test]
fn a_rebase_is_told_step_by_step_as_git_tells_it() {
    let pair = branches();
    pair.write("m", "m\n");
    pair.commit_all_at("main moves on", T + 3);
    let onto = id(&pair, "main");
    pair.both(&["switch", "side"]);
    pair.both(&["rebase", &onto]);
    for args in [&["reflog", "-n", "4"][..], &["reflog", "side", "-n", "2"]] {
        let (ours, theirs) = (ok(alt(&pair.alt, args)), ok(git(&pair.git, args)));
        assert_eq!(told(&ours), told(&theirs), "{args:?}");
    }
    let log = ok(alt(&pair.alt, &["reflog", "-n", "3"]));
    let started = format!("HEAD@{{2}}: rebase (start): checkout {onto}");
    assert_eq!(told(&log)[2], started);
    // the steps of a rebase are no branches left
    assert_eq!(
        ok(alt(&pair.alt, &["rev-parse", "--abbrev-ref", "@{-1}"])),
        "main\n"
    );
}

#[test]
fn a_revision_names_what_a_ref_was_at_and_the_branch_left() {
    let pair = moved();
    let moves = pair.same(&["reflog"]).lines().count();
    for back in 0..moves {
        pair.same(&["rev-parse", &format!("HEAD@{{{back}}}")]);
    }
    for spec in [
        "@{0}",
        "@{1}",
        "@{4}",
        "main@{2}",
        "main@{6}",
        "other@{1}",
        "side@{0}",
        "refs/heads/on@{1}",
        "@{-1}",
        "@{-2}",
        "@{-3}",
        "@{-4}",
        "HEAD@{2}~1",
        "@{-1}~1",
        "@{-2}^{tree}",
        "main@{1}:a",
        "@{-1}@{1}",
    ] {
        pair.same(&["rev-parse", spec]);
    }
    for back in ["@{-1}", "@{-2}", "@{-3}"] {
        pair.same(&["rev-parse", "--abbrev-ref", back]);
        pair.same(&["rev-parse", "--symbolic-full-name", back]);
    }
    pair.same(&["log", "--oneline", "-n", "2", "@{-1}"]);
    pair.same(&["log", "--oneline", "HEAD@{1}..HEAD"]);
    pair.same(&["diff", "--name-status", "HEAD@{3}", "HEAD"]);
}

#[test]
fn a_log_asked_past_its_end_and_a_branch_never_left_are_refused() {
    let fresh = Pair::new();
    of_late::same_failure(
        &fresh,
        &["switch", "-"],
        128,
        "fatal: invalid reference: @{-1}\n",
    );
    let none = "fatal: your current branch 'main' does not have any commits yet\n";
    of_late::same_failure(&fresh, &["reflog"], 128, none);

    let pair = moved();
    let moves = pair.same(&["reflog"]).lines().count();
    let past = format!("fatal: log for 'HEAD' only has {moves} entries\n");
    of_late::same_failure(&pair, &["rev-parse", "HEAD@{99}"], 128, &past);
    let of_main = pair.same(&["reflog", "main"]).lines().count();
    let past = format!("fatal: log for 'main' only has {of_main} entries\n");
    of_late::same_failure(&pair, &["rev-parse", "main@{99}"], 128, &past);
    of_late::same_failure(&pair, &["rev-parse", "@{99}"], 128, &past);
    let unknown = |spec: &str| {
        format!(
            "fatal: ambiguous argument '{spec}': unknown revision or path not in the working tree.\n\
             Use '--' to separate paths from revisions, like this:\n\
             'git <command> [<revision>...] -- [<file>...]'\n"
        )
    };
    for spec in ["@{-99}", "nosuch@{1}"] {
        of_late::same_failure(&pair, &["rev-parse", spec], 128, &unknown(spec));
    }
    of_late::same_failure(&pair, &["reflog", "nosuch"], 128, &unknown("nosuch"));

    // a date in the braces is not read, and is said not to be
    let said = fails(alt(&pair.alt, &["rev-parse", "HEAD@{yesterday}"]), 128);
    assert!(said.contains("a date is not read there"), "{said}");
    pair.assert_same_state();
}

#[test]
fn a_pick_is_told_as_a_pick() {
    let pair = branches();
    std::fs::write(pair.alt.join("x"), "x\n").unwrap();
    ok(alt(&pair.alt, &["add", "x"]));
    ok(alt(&pair.alt, &["commit", "-m", "x"]));
    ok(alt(&pair.alt, &["cherry-pick", &id(&pair, "side")]));
    let log = ok(alt(&pair.alt, &["reflog", "-n", "1"]));
    assert!(log.ends_with(" HEAD@{0}: cherry-pick: side one\n"), "{log}");
}

#[test]
fn an_undo_is_a_move_the_log_tells_of() {
    let pair = branches();
    std::fs::write(pair.alt.join("a"), "3\n").unwrap();
    ok(alt(&pair.alt, &["add", "a"]));
    ok(alt(&pair.alt, &["commit", "-m", "third"]));
    let parse = |spec: &str| ok(alt(&pair.alt, &["rev-parse", spec]));
    let (third, second) = (parse("HEAD"), parse("HEAD~1"));
    ok(alt(&pair.alt, &["undo"]));
    assert_eq!(parse("HEAD"), second);
    // the commit taken back is where the ref was one move ago
    assert_eq!(parse("HEAD@{1}"), third);
    assert_eq!(parse("main@{1}"), third);
    let log = ok(alt(&pair.alt, &["reflog", "-n", "2"]));
    let lines: Vec<&str> = log.lines().collect();
    assert!(
        lines[0].ends_with(&format!("HEAD@{{0}}: undo: moving to {}", second.trim())),
        "{log}"
    );
    assert!(lines[1].ends_with("HEAD@{1}: commit: third"), "{log}");

    // a switch taken back leaves the branch it had gone to
    ok(alt(&pair.alt, &["switch", "side"]));
    ok(alt(&pair.alt, &["undo"]));
    let left = ok(alt(&pair.alt, &["rev-parse", "--abbrev-ref", "@{-1}"]));
    assert_eq!(left, "side\n");
    let said = ok(alt(&pair.alt, &["switch", "-"]));
    assert!(said.ends_with("Switched to branch 'side'\n"), "{said}");
}

#[test]
fn the_daemon_and_a_parallel_workspace_read_the_log_alike() {
    let pair = moved();
    for args in [
        &["reflog"][..],
        &["reflog", "main", "-n", "4"],
        &["reflog", "--json", "-n", "2"],
        &["rev-parse", "HEAD@{1}", "@{-1}", "@{2}", "main@{3}"],
        &["rev-parse", "--abbrev-ref", "@{-2}"],
    ] {
        assert_eq!(
            ok(alt_daemon(&pair.alt, args)),
            ok(alt(&pair.alt, args)),
            "{args:?}"
        );
    }
    let json = ok(alt(&pair.alt, &["reflog", "--json", "-n", "1"]));
    assert!(json.contains("\"selector\":\"HEAD@{0}\""), "{json}");
    assert!(
        json.contains("\"message\":\"revert: Revert \\\"amended\\\"\""),
        "{json}"
    );
    let said = ok(alt_daemon(&pair.alt, &["switch", "-"]));
    ok(git(&pair.git, &["switch", "-"]));
    assert_eq!(said, "Switched to branch 'on'\n");
    pair.assert_same_state();

    // a workspace has the log of its own head
    let before = ok(alt(&pair.alt, &["reflog"]));
    ok(alt(&pair.alt, &["branch", "third"]));
    let trees = alt_os::temp::tempdir().unwrap();
    let tree = trees.path().join("ws");
    let there = |args: &[&str]| {
        let mut all = vec!["--workspace", "ws"];
        all.extend(args);
        ok(alt(&pair.alt, &all))
    };
    ok(alt(
        &pair.alt,
        &["workspace", "add", "ws", tree.to_str().unwrap(), "third"],
    ));
    there(&["switch", "side"]);
    there(&["switch", "third"]);
    assert_eq!(there(&["rev-parse", "--abbrev-ref", "@{-1}"]), "side\n");
    assert_eq!(
        there(&["rev-parse", "HEAD@{1}"]),
        there(&["rev-parse", "side"])
    );
    let log = there(&["reflog", "-n", "2"]);
    let lines: Vec<&str> = log.lines().collect();
    assert!(
        lines[0].ends_with("HEAD@{0}: checkout: moving from side to third"),
        "{log}"
    );
    assert!(
        lines[1].ends_with("HEAD@{1}: checkout: moving from third to side"),
        "{log}"
    );
    assert_eq!(there(&["switch", "-"]), "Switched to branch 'side'\n");
    assert_eq!(ok(alt(&pair.alt, &["reflog"])), before);
    assert_eq!(
        ok(alt(&pair.alt, &["rev-parse", "--abbrev-ref", "@{-1}"])),
        "main\n"
    );
}

/// A git repository whose head went from branch to branch, and the
/// directory it is imported into.
fn imported() -> (
    alt_os::temp::TempDir,
    std::path::PathBuf,
    std::path::PathBuf,
) {
    let dir = alt_os::temp::tempdir().unwrap();
    let (source, to) = (dir.path().join("git"), dir.path().join("alt"));
    std::fs::create_dir(&source).unwrap();
    ok(git(&source, &["init", "-q", "."]));
    let commit = |name: &str, message: &str| {
        std::fs::write(source.join(name), message).unwrap();
        ok(git(&source, &["add", "."]));
        ok(git(&source, &["commit", "-q", "-m", message]));
    };
    commit("a", "one");
    commit("a", "two");
    ok(git(&source, &["switch", "-q", "-c", "side"]));
    commit("s", "on side");
    ok(git(&source, &["switch", "-q", "main"]));
    ok(git(&source, &["reset", "-q", "--hard", "HEAD~1"]));
    ok(alt(&source, &["import", to.to_str().unwrap()]));
    (dir, source, to)
}

fn same_logs(ours: &Path, theirs: &Path) {
    for args in [
        &["reflog"][..],
        &["reflog", "main"],
        &["reflog", "side"],
        &[
            "rev-parse",
            "HEAD@{1}",
            "HEAD@{3}",
            "main@{1}",
            "@{1}",
            "@{-1}",
            "side@{1}",
        ],
        &["rev-parse", "--abbrev-ref", "@{-1}"],
    ] {
        assert_eq!(ok(alt(ours, args)), ok(git(theirs, args)), "{args:?}");
    }
}

#[test]
fn an_import_brings_the_logs_git_kept_and_an_export_gives_them_back() {
    let (_dir, source, to) = imported();
    same_logs(&to, &source);
    let out = export(&to);
    let exported = out.path().join("exported");
    for log in ["logs/HEAD", "logs/refs/heads/main", "logs/refs/heads/side"] {
        let read = |repo: &Path| std::fs::read(repo.join(".git").join(log)).unwrap();
        assert_eq!(read(&exported), read(&source), "{log}");
    }

    // what is done here goes on from where git left the logs
    ok(alt(&to, &["switch", "side"]));
    std::fs::write(to.join("s"), "more").unwrap();
    ok(alt(&to, &["add", "s"]));
    ok(alt(&to, &["commit", "-m", "more on side"]));
    ok(alt(&to, &["switch", "-"]));
    let log = ok(alt(&to, &["reflog"]));
    assert!(
        log.contains("HEAD@{0}: checkout: moving from side to main\n"),
        "{log}"
    );
    assert!(log.contains("HEAD@{1}: commit: more on side\n"), "{log}");
    assert!(log.contains("HEAD@{3}: reset: moving to HEAD~1\n"), "{log}");
    let out = export(&to);
    let exported = out.path().join("exported");
    same_logs(&to, &exported);

    // and git goes on from there, which the next import reads whole
    ok(git(&exported, &["checkout", "-q", "main", "--", "."]));
    ok(git(&exported, &["switch", "-q", "side"]));
    std::fs::write(exported.join("s"), "by git").unwrap();
    ok(git(&exported, &["commit", "-q", "-a", "-m", "by git"]));
    ok(alt(&exported, &["import", to.to_str().unwrap()]));
    for args in [
        &["reflog", "side"][..],
        &["rev-parse", "side@{1}", "side@{2}"],
    ] {
        assert_eq!(ok(alt(&to, args)), ok(git(&exported, args)), "{args:?}");
    }
}
