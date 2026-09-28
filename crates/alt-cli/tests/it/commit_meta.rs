//! Commit metadata in git notes: written with every commit, editable later
//! without touching the commit, readable by git itself after export, and
//! carried along when amend or rebase replaces a commit.

use std::path::Path;
use std::process::{Command, Output};

fn alt_env(repo: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_alt"));
    c.current_dir(repo)
        .env("ALT_NO_DAEMON", "1")
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .env("GIT_EDITOR", "true")
        .args(args);
    for (k, v) in env {
        c.env(k, v);
    }
    c.output().unwrap()
}

fn alt(repo: &Path, args: &[&str]) -> Output {
    alt_env(repo, args, &[])
}

fn git(repo: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args(args)
        .output()
        .unwrap()
}

fn ok(o: Output) -> String {
    assert!(
        o.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout).unwrap()
}

fn commit_file(root: &Path, file: &str, body: &str, args: &[&str], env: &[(&str, &str)]) {
    std::fs::write(root.join(file), body).unwrap();
    ok(alt_env(root, &["add", "."], env));
    ok(alt_env(root, &[&["commit"][..], args].concat(), env));
}

fn init() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    ok(alt(dir.path(), &["init", "."]));
    dir
}

#[test]
fn every_commit_gets_derived_metadata_plus_what_was_recorded() {
    let dir = init();
    let root = dir.path();
    commit_file(root, "a.txt", "a\n", &["-m", "chore: base"], &[]);
    commit_file(
        root,
        "src.rs",
        "fn f() {}\n",
        &[
            "-m",
            "feat(parser)!: handle Result types",
            "--decision",
            "kept the old API behind a flag",
            "--meta",
            "model=m-7",
        ],
        &[],
    );
    let note = ok(alt(root, &["meta", "show"]));
    for line in [
        "schema: alt-meta/1",
        "type: feat",
        "summary: handle Result types",
        "breaking: true",
        "author-type: human",
        "touched: src.rs",
        "decision: kept the old API behind a flag",
        "x-model: m-7",
    ] {
        assert!(
            note.lines().any(|l| l == line),
            "missing {line:?} in:\n{note}"
        );
    }
    assert!(!note.contains("touched: a.txt"), "{note}");

    std::fs::write(root.join("b.txt"), "b\n").unwrap();
    ok(alt(root, &["add", "."]));
    let too_many = [
        "--decision",
        "1",
        "--decision",
        "2",
        "--decision",
        "3",
        "--decision",
        "4",
    ];
    let o = alt(root, &[&["commit", "-m", "x"][..], &too_many].concat());
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("at most 3 decisions"));
}

#[test]
fn an_agent_commit_is_authored_by_the_agent_and_names_who_it_acts_for() {
    let dir = init();
    let root = dir.path();
    let agent = [
        ("ALT_PRINCIPAL_KIND", "agent"),
        ("ALT_PRINCIPAL_ID", "bot-a"),
        ("ALT_CONTROLLING", "owner@example.org"),
        ("ALT_SESSION_ID", "s-42"),
        ("GIT_AUTHOR_NAME", "bot-a"),
        ("GIT_AUTHOR_EMAIL", "bot-a@agents.local"),
    ];
    commit_file(
        root,
        "a.txt",
        "a\n",
        &["-m", "feat: from the agent"],
        &agent,
    );
    let note = ok(alt(root, &["meta", "show"]));
    for line in [
        "author-type: agent",
        "author: bot-a",
        "controlling: owner@example.org",
        "session: s-42",
    ] {
        assert!(
            note.lines().any(|l| l == line),
            "missing {line:?} in:\n{note}"
        );
    }
    let head = ok(alt(root, &["cat-file", "-p", "HEAD"]));
    assert!(
        head.contains("\nauthor bot-a <bot-a@agents.local> "),
        "{head}"
    );
}

#[test]
fn a_replacement_message_changes_no_commit() {
    let dir = init();
    let root = dir.path();
    commit_file(root, "a.txt", "a\n", &["-m", "feat: frist"], &[]);
    commit_file(root, "b.txt", "b\n", &["-m", "feat: second"], &[]);
    let before = ok(alt(root, &["log", "--pretty=raw"]));
    let tip = ok(alt(root, &["rev-parse", "HEAD"]));

    ok(alt(
        root,
        &["meta", "set", "HEAD~1", "-m", "feat: first, spelled right"],
    ));
    assert_eq!(
        ok(alt(root, &["rev-parse", "HEAD"])),
        tip,
        "no commit may change"
    );
    assert_eq!(
        ok(alt(root, &["log", "--pretty=raw"])),
        before,
        "raw shows the objects"
    );
    let oneline = ok(alt(root, &["log", "--pretty=oneline"]));
    assert!(
        oneline
            .lines()
            .nth(1)
            .unwrap()
            .ends_with(" feat: first, spelled right"),
        "{oneline}"
    );
    let json = ok(alt(root, &["log", "--json", "-n", "2"]));
    assert!(
        json.contains("\"note_message\":\"feat: first, spelled right\\n\""),
        "{json}"
    );
    assert!(json.contains("\"message\":\"feat: frist\\n\""), "{json}");
}

#[test]
fn git_reads_the_notes_after_export_and_the_ids_match() {
    let dir = init();
    let root = dir.path();
    commit_file(
        root,
        "a.txt",
        "a\n",
        &["-m", "feat: a", "--decision", "one step"],
        &[],
    );
    let tip = ok(alt(root, &["rev-parse", "HEAD"]));
    let out = tempfile::tempdir().unwrap();
    let exported = out.path().join("g");
    ok(alt(root, &["export", exported.to_str().unwrap()]));
    ok(git(&exported, &["fsck", "--strict"]));
    assert_eq!(ok(git(&exported, &["rev-parse", "HEAD"])), tip);
    let note = ok(git(&exported, &["notes", "--ref=alt/meta", "show", "HEAD"]));
    assert!(note.contains("decision: one step"), "{note}");
    // plain git log shows the commit alone: notes under alt/meta are opt-in
    let log = ok(git(&exported, &["log", "-1"]));
    assert!(!log.contains("decision"), "{log}");
}

#[test]
fn amend_and_rebase_carry_recorded_fields_to_the_new_commit() {
    let dir = init();
    let root = dir.path();
    commit_file(root, "a.txt", "a\n", &["-m", "chore: base"], &[]);
    ok(alt(root, &["branch", "topic"]));
    ok(alt(root, &["switch", "topic"]));
    commit_file(
        root,
        "t.txt",
        "t\n",
        &["-m", "feat: t", "--decision", "why t"],
        &[],
    );
    ok(alt(root, &["meta", "set", "-m", "feat: t, described"]));

    std::fs::write(root.join("t.txt"), "t2\n").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "--amend", "--no-edit"]));
    let note = ok(alt(root, &["meta", "show"]));
    assert!(
        note.contains("decision: why t") && note.contains("feat: t, described"),
        "{note}"
    );

    ok(alt(root, &["switch", "main"]));
    commit_file(root, "m.txt", "m\n", &["-m", "chore: move main"], &[]);
    ok(alt(root, &["switch", "topic"]));
    ok(alt(root, &["rebase", "main"]));
    let note = ok(alt(root, &["meta", "show"]));
    assert!(
        note.contains("decision: why t") && note.contains("feat: t, described"),
        "{note}"
    );
}
