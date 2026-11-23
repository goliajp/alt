//! Every command there is, with arguments it parses with. Which of them
//! only read is asked of the program itself.

use std::ffi::OsString;

/// `{request}` stands for the id of a request the repository has on
/// record, `{out}` for a directory outside it that does not exist.
pub(super) const COMMANDS: &[&[&str]] = &[
    &["cat-file", "-p", "HEAD"],
    &["cat-file", "-t", "HEAD"],
    &["cat-file", "-s", "HEAD"],
    &["rev-parse", "HEAD"],
    &[
        "rev-parse",
        "--abbrev-ref",
        "--short",
        "HEAD",
        ":a.txt",
        "HEAD:",
    ],
    &["rev-parse", "--show-toplevel", "--git-dir"],
    &["log"],
    &["log", "--oneline", "HEAD..HEAD", "--", "a.txt"],
    &["log", "-n", "1"],
    &["log", "-p", "-n", "2"],
    &["log", "--json"],
    &["show"],
    &["show", "--json"],
    &["show", "HEAD:a.txt"],
    &["blame", "a.txt"],
    &["status"],
    &["status", "--json"],
    &["shell", "zsh"],
    &["prompt"],
    &["prompt", "--format=zsh"],
    &[
        "complete",
        "--shell",
        "zsh",
        "--current",
        "2",
        "--",
        "alt",
        "show",
        "",
    ],
    &[
        "complete",
        "--shell",
        "zsh",
        "--current",
        "2",
        "--",
        "alt",
        "add",
        "",
    ],
    &[
        "complete",
        "--shell",
        "zsh",
        "--current",
        "2",
        "--",
        "alt",
        "blame",
        "",
    ],
    &["diff"],
    &["diff", "--cached"],
    &["diff", "--json"],
    &["diff", "--stat", "HEAD"],
    &["diff", "--name-status", "HEAD", "HEAD", "--", "a.txt"],
    &["stash", "list"],
    &["stash", "show"],
    &["branch"],
    &["branch", "--json"],
    &["tag"],
    &["tag", "--json"],
    &["commit", "--validate"],
    &["meta", "show", "HEAD"],
    &["lfs", "ls"],
    &["config", "--list"],
    &["config", "user.name"],
    &["op-log"],
    &["op-log", "-n", "3", "--json"],
    &["op-log", "--verify"],
    &["verify"],
    &["fsck"],
    &["operation", "pending"],
    &["operation", "status", "00"],
    &["request", "pending"],
    &["request", "status", "{request}"],
    &["request", "result", "{request}"],
    &["workspace", "list"],
    &["remote", "list"],
    &["identity", "list"],
    &["task", "list"],
    &["task", "show", "t"],
    &["task", "changes", "c"],
    &["task", "review-show", "r"],
    &["task", "integration-show", "i"],
    &["task", "delivery-show", "d"],
    &["task", "remote", "status", "x"],
    &["task", "remote", "changes", "channels", "r"],
    &["task", "remote", "changes", "list", "r"],
    &["task", "remote", "changes", "show", "r", "d"],
    &["task", "remote", "changes", "status", "x"],
    &["candidate", "show", "c"],
    &["candidate", "check", "c"],
    &["evidence", "show", "e"],
    &["evidence", "check", "e"],
    &["evidence", "log", "e", "stdout"],
    &["evidence", "run-status", "e"],
    &["export", "{out}"],
    &["ci", "validate"],
    // the rest write
    &["init"],
    &["import", "x"],
    &["intake", "x", "y"],
    &["clone", "x"],
    &["add", "a.txt"],
    &["commit", "-m", "x"],
    &["switch", "x"],
    &["restore", "a.txt"],
    &["checkout", "x"],
    &["reset"],
    &["rm", "a.txt"],
    &["mv", "a.txt", "b.txt"],
    &["stash"],
    &["stash", "pop"],
    &["branch", "x"],
    &["tag", "x"],
    &["meta", "set", "HEAD", "-m", "x"],
    &["lfs", "fetch"],
    &["bisect", "reset"],
    &["rebase", "x"],
    &["revert", "HEAD"],
    &["cherry-pick", "HEAD"],
    &["config", "user.name", "x"],
    &["merge", "x"],
    &["flow", "init"],
    &["undo"],
    &["operation", "recover", "00"],
    &["workspace", "add", "x", "y"],
    &["remote", "add", "x", "y"],
    &["fetch", "x"],
    &["push", "x"],
    &["identity", "init", "x"],
    &["task", "abandon", "t", "--reason", "x"],
    &["candidate", "create", "t"],
    &["evidence", "collect", "e"],
];

/// Commands that run before the repository's store is looked for: they
/// keep journals of their own and talk to a server or a runner.
pub(super) const APART: [&str; 3] = ["portable", "auth", "runner"];

/// Whether the program takes `args` for a command that only reads.
pub(super) fn only_reads(args: &[&str]) -> bool {
    let words = std::iter::once("alt").chain(args.iter().copied());
    let parsed = alt_cli::machine::parse(words.map(OsString::from).collect());
    let Ok(parsed) = parsed else {
        panic!("{args:?} does not parse");
    };
    alt_cli::native::only_reads(&parsed.cli.command)
}

/// The commands of [`COMMANDS`] that only read.
pub(super) fn reads() -> Vec<&'static [&'static str]> {
    let reads: Vec<_> = COMMANDS
        .iter()
        .copied()
        .filter(|args| only_reads(args))
        .collect();
    reads
}
