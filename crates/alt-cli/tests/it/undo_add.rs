//! M8-B1: `alt undo` covers `alt add` too. The op log gets an
//! `PAYLOAD_INDEX_TX` entry per `add`; `undo` parses that kind, restores
//! the index entries to their prior state, and records an inverse op so
//! the second `undo` brings the staging back. Extends the M4 ref-tx undo
//! into VISION A2's "any state-changing op is reversible".

use std::path::Path;
use std::process::{Command, Output};

fn alt(repo: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_alt"))
        .current_dir(repo)
        .env("ALT_NO_DAEMON", "1")
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .env("USER", "tester")
        .args(args)
        .output()
        .unwrap()
}

fn ok(o: Output) -> String {
    assert!(
        o.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout).unwrap()
}

fn staged_paths(repo: &Path) -> String {
    ok(alt(repo, &["status", "--json"]))
}

#[test]
fn undo_unstages_a_single_alt_add() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("a.txt"), "alpha\n").unwrap();
    ok(alt(root, &["add", "a.txt"]));

    let after_add = staged_paths(root);
    assert!(
        after_add.contains("\"a.txt\""),
        "staged set must contain a.txt after add: {after_add}"
    );

    // Undo the add — the index loses the entry; status reports the file
    // back as untracked (no head yet, so it was a pure add).
    ok(alt(root, &["undo"]));
    let after_undo = staged_paths(root);
    assert!(
        !after_undo.contains("\"staged\":[{\"path\":\"a.txt\""),
        "a.txt must drop out of the staged column after undo: {after_undo}"
    );
    assert!(
        after_undo.contains("\"untracked\":[\"a.txt\"]"),
        "a.txt re-surfaces as untracked after undo: {after_undo}"
    );
}

#[test]
fn undo_undoes_only_the_last_add_when_there_are_two() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("a.txt"), "alpha\n").unwrap();
    std::fs::write(root.join("b.txt"), "beta\n").unwrap();
    ok(alt(root, &["add", "a.txt"]));
    ok(alt(root, &["add", "b.txt"]));

    let after_both = staged_paths(root);
    assert!(after_both.contains("\"a.txt\"") && after_both.contains("\"b.txt\""));

    // One undo: only `b.txt` falls out; `a.txt` is still staged.
    ok(alt(root, &["undo"]));
    let after_one = staged_paths(root);
    assert!(
        after_one.contains("\"a.txt\""),
        "a.txt must still be staged after one undo: {after_one}"
    );
    assert!(
        after_one.contains("\"untracked\":[\"b.txt\"]"),
        "b.txt back to untracked after one undo: {after_one}"
    );
}

#[test]
fn undo_of_undo_redoes_the_add() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("a.txt"), "alpha\n").unwrap();
    ok(alt(root, &["add", "a.txt"]));
    ok(alt(root, &["undo"]));
    // The first undo recorded its own inverse index-tx op, so a second
    // undo replays the original add — staged again, no manual restage.
    ok(alt(root, &["undo"]));
    let after_redo = staged_paths(root);
    assert!(
        after_redo.contains("\"a.txt\""),
        "a.txt must be staged again after undo-of-undo: {after_redo}"
    );
}

#[test]
fn undo_after_commit_still_inverts_the_ref_tx_not_the_index() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("a.txt"), "alpha\n").unwrap();
    ok(alt(root, &["add", "a.txt"]));
    ok(alt(root, &["commit", "-m", "first"]));

    // The last op is now a commit (ref tx). Undo must invert the ref
    // change — moves HEAD back to unborn, drops a.txt from the working
    // tree (the checkout step restores the prior tree, which was empty).
    ok(alt(root, &["undo"]));
    // Log should no longer find any commit on main.
    let log_out = alt(root, &["log", "--pretty=oneline"]);
    let log = String::from_utf8_lossy(&log_out.stdout);
    let stderr = String::from_utf8_lossy(&log_out.stderr);
    assert!(
        log.trim().is_empty() || stderr.contains("bad revision") || stderr.contains("unborn"),
        "after undoing the only commit, log must be empty or report unborn HEAD: stdout={log} stderr={stderr}",
    );
}
