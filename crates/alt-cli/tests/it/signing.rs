//! Whom a commit, an annotated tag and a notes commit are written by: the
//! environment before the configuration, the author apart from the
//! committer, and nobody made up where neither names one. Run directly
//! and through the daemon, and beside git where git has the same rule.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The variables that name somebody; a test starts from none of them.
const NAMING: [&str; 8] = [
    "GIT_AUTHOR_NAME",
    "GIT_AUTHOR_EMAIL",
    "GIT_AUTHOR_DATE",
    "GIT_COMMITTER_NAME",
    "GIT_COMMITTER_EMAIL",
    "GIT_COMMITTER_DATE",
    "EMAIL",
    "TZ",
];

mod beside_git;
mod zones;

type Env<'a> = &'a [(&'a str, &'a str)];

fn alt(root: &Path, daemon: bool, env: Env, args: &[&str]) -> Output {
    let mut alt = crate::alt_command();
    for name in NAMING {
        alt.env_remove(name);
    }
    alt.current_dir(root)
        .env("USER", "someone")
        .envs(env.iter().copied());
    let idle = env.iter().any(|(name, _)| *name == "ALT_DAEMON_IDLE_MS");
    match daemon {
        true if idle => &mut alt,
        true => alt.env("ALT_DAEMON_IDLE_MS", "1500"),
        false => alt.env("ALT_NO_DAEMON", "1"),
    };
    let out = alt.args(args).output().unwrap();
    if daemon && args[0] == "commit" {
        let socket = root.join(".alt/daemon.sock");
        assert!(socket.exists(), "the commit did not go through a daemon");
    }
    out
}

fn git(root: &Path, env: Env, args: &[&str]) -> Output {
    let mut git = Command::new("git");
    for name in NAMING {
        git.env_remove(name);
    }
    git.current_dir(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("HOME", root)
        .env("TZ", "UTC")
        .envs(env.iter().copied());
    git.args(args).output().unwrap()
}

fn ok(out: Output) -> String {
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "stderr: {stderr}");
    String::from_utf8(out.stdout).unwrap()
}

/// The `author`, `committer` and `tagger` lines of what `rev` names.
fn alt_lines(root: &Path, rev: &str) -> Vec<String> {
    by_lines(&ok(alt(root, false, &[], &["cat-file", "-p", rev])))
}

fn git_lines(root: &Path, kind: &str, rev: &str) -> Vec<String> {
    by_lines(&ok(git(root, &[], &["cat-file", kind, rev])))
}

fn by_lines(object: &str) -> Vec<String> {
    let headers = object.lines().take_while(|line| !line.is_empty());
    let by = |line: &&str| {
        ["author ", "committer ", "tagger "]
            .iter()
            .any(|h| line.starts_with(h))
    };
    headers.filter(by).map(str::to_owned).collect()
}

/// The lines with the time taken off, for a commit made at no set time.
fn who(lines: &[String]) -> Vec<&str> {
    let named = lines.iter().map(|line| line.rsplitn(3, ' ').last());
    named.map(Option::unwrap).collect()
}

/// A repository with one commit, so that a commit of nothing may follow.
fn init(root: &Path) {
    ok(alt(root, false, &[], &["init", "."]));
    std::fs::write(root.join("seed"), "seed").unwrap();
    ok(alt(root, false, &[], &["add", "seed"]));
    let by = [
        ("GIT_AUTHOR_NAME", "Seed"),
        ("GIT_AUTHOR_EMAIL", "seed@x"),
        ("GIT_COMMITTER_NAME", "Seed"),
        ("GIT_COMMITTER_EMAIL", "seed@x"),
    ];
    ok(alt(root, false, &by, &["commit", "-m", "seed"]));
}

fn set(root: &Path, daemon: bool, key: &str, value: &str) {
    ok(alt(root, daemon, &[], &["config", key, value]));
}

fn commit(root: &Path, daemon: bool, env: Env) -> Output {
    alt(
        root,
        daemon,
        env,
        &["commit", "--allow-empty", "-m", "made"],
    )
}

const DATES: [(&str, &str); 2] = [
    ("GIT_AUTHOR_DATE", "1700000000 +0900"),
    ("GIT_COMMITTER_DATE", "2005-04-07T22:13:13+02:00"),
];

/// Everything of the repository a refused command must leave as it was:
/// the refs, the index, the operation log, what is in progress, and every
/// file under `.alt` but those a request leaves of itself, the daemon's
/// own, the locks, and the marks of what is synced, which a daemon moves
/// after the command that wrote is answered.
fn state(root: &Path) -> Vec<String> {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let read = |args: &[&str]| {
        let out = alt(root, false, &[], args);
        format!("{args:?} {out:?}")
    };
    let log = ok(alt(root, false, &[], &["op-log", "--json"]));
    let mut state = vec![
        format!("operations {}", log.matches("\"timestamp_ms\"").count()),
        read(&["branch", "--json"]),
        read(&["tag"]),
        read(&["rev-parse", "HEAD"]),
        read(&["rev-parse", "refs/notes/alt/meta"]),
        read(&["status"]),
        read(&["operation", "pending"]),
    ];
    let passed = |name: &str| {
        let own = [
            "requests",
            "submissions",
            "index.scan",
            "index.seen",
            "odb.durable",
            "durable",
        ]
        .contains(&name);
        own || name.starts_with("daemon.") || name.ends_with(".lock")
    };
    let mut dirs: Vec<PathBuf> = vec![root.join(".alt")];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(dir).unwrap() {
            let (path, name) = entry
                .map(|entry| (entry.path(), entry.file_name()))
                .unwrap();
            if passed(&name.to_string_lossy()) {
                continue;
            }
            if path.is_dir() {
                dirs.push(path);
                continue;
            }
            let bytes = std::fs::read(&path).unwrap();
            let mut hasher = DefaultHasher::new();
            bytes.hash(&mut hasher);
            let name = path.strip_prefix(root).unwrap();
            state.push(format!("{name:?} {} {:x}", bytes.len(), hasher.finish()));
        }
    }
    state.sort();
    state
}

#[test]
fn the_configuration_of_the_repository_names_both() {
    for daemon in [false, true] {
        let dir = alt_os::temp::tempdir().unwrap();
        let root = dir.path();
        init(root);
        set(root, daemon, "user.name", "Repo User");
        set(root, daemon, "user.email", "repo@example.com");
        ok(commit(root, daemon, &[]));
        let lines = alt_lines(root, "HEAD");
        let by = "Repo User <repo@example.com>";
        assert_eq!(
            who(&lines),
            [format!("author {by}"), format!("committer {by}")]
        );
        // the commit that records the metadata is the committer's
        let other = [
            ("GIT_AUTHOR_NAME", "Other"),
            ("GIT_AUTHOR_EMAIL", "other@x"),
        ];
        ok(commit(root, daemon, &other));
        let lines = alt_lines(root, "HEAD");
        assert_eq!(who(&lines)[0], "author Other <other@x>");
        let notes = alt_lines(root, "refs/notes/alt/meta");
        assert_eq!(
            who(&notes),
            [format!("author {by}"), format!("committer {by}")]
        );
    }
}

#[test]
fn the_configuration_outside_the_repository_names_both() {
    for daemon in [false, true] {
        let dir = alt_os::temp::tempdir().unwrap();
        let root = dir.path().join("repo");
        std::fs::create_dir(&root).unwrap();
        init(&root);
        let global = dir.path().join("global");
        std::fs::write(
            &global,
            "[user]\n\tname = Global User\n\temail = global@x\n",
        )
        .unwrap();
        let named = [("GIT_CONFIG_GLOBAL", global.to_str().unwrap())];
        ok(commit(&root, daemon, &named));
        let by = "Global User <global@x>";
        let both = [format!("author {by}"), format!("committer {by}")];
        assert_eq!(who(&alt_lines(&root, "HEAD")), both);

        // the file in the caller's home, where no variable names another
        let home = dir.path().join("home");
        std::fs::create_dir(&home).unwrap();
        std::fs::write(
            home.join(".gitconfig"),
            "[user]\n\tname = Home\n\temail = home@x\n",
        )
        .unwrap();
        let mut alt = crate::alt_command();
        for name in NAMING {
            alt.env_remove(name);
        }
        alt.env_remove("GIT_CONFIG_GLOBAL")
            .env_remove("XDG_CONFIG_HOME");
        alt.env("HOME", &home).current_dir(&root);
        match daemon {
            true => alt.env("ALT_DAEMON_IDLE_MS", "1500"),
            false => alt.env("ALT_NO_DAEMON", "1"),
        };
        ok(alt
            .args(["commit", "--allow-empty", "-m", "at home"])
            .output()
            .unwrap());
        let by = "Home <home@x>";
        let both = [format!("author {by}"), format!("committer {by}")];
        assert_eq!(who(&alt_lines(&root, "HEAD")), both);
    }
}

#[test]
fn a_value_with_nothing_in_it_is_passed_over() {
    for daemon in [false, true] {
        let dir = alt_os::temp::tempdir().unwrap();
        let root = dir.path();
        init(root);
        set(root, daemon, "user.name", "U");
        set(root, daemon, "user.email", "u@x");
        let blank = [
            ("GIT_AUTHOR_NAME", "  "),
            ("GIT_COMMITTER_EMAIL", ""),
            ("GIT_AUTHOR_DATE", ""),
        ];
        ok(commit(root, daemon, &blank));
        assert_eq!(
            who(&alt_lines(root, "HEAD")),
            ["author U <u@x>", "committer U <u@x>"]
        );
    }
}

/// A repository of three branches, `main`, `topic` and `side`, each with
/// a commit the others lack on top of the one they share, all by First.
/// `topic` and `main` change either end of the one file `shared`, so
/// that joining them writes a file neither has.
fn diverged(root: &Path, run: &dyn Fn(Env, &[&str])) {
    let first = [
        ("GIT_AUTHOR_NAME", "First"),
        ("GIT_AUTHOR_EMAIL", "first@x"),
        ("GIT_COMMITTER_NAME", "First"),
        ("GIT_COMMITTER_EMAIL", "first@x"),
        ("GIT_AUTHOR_DATE", "1600000100 +0200"),
        ("GIT_COMMITTER_DATE", "1600000200 +0200"),
    ];
    let commit = |file: &str, ends: [&str; 2]| {
        let [top, bottom] = ends;
        let shared = format!("{top}\n2\n3\n4\n5\n6\n7\n8\n{bottom}\n");
        std::fs::write(root.join("shared"), shared).unwrap();
        std::fs::write(root.join(file), file).unwrap();
        run(&[], &["add", "."]);
        run(&first, &["commit", "-m", file]);
    };
    commit("base", ["1", "9"]);
    let branches = [
        ("topic", "theirs", ["one", "9"]),
        ("side", "aside", ["1", "9"]),
        ("main", "ours", ["1", "nine"]),
    ];
    for (branch, file, ends) in branches {
        if branch != "main" {
            run(&[], &["branch", branch]);
        }
        run(&[], &["switch", branch]);
        commit(file, ends);
        run(&[], &["switch", "main"]);
    }
}

const LATER: [(&str, &str); 6] = [
    ("GIT_AUTHOR_NAME", "Second"),
    ("GIT_AUTHOR_EMAIL", "second@x"),
    ("GIT_COMMITTER_NAME", "Third"),
    ("GIT_COMMITTER_EMAIL", "third@x"),
    ("GIT_AUTHOR_DATE", "1700000000 +0900"),
    ("GIT_COMMITTER_DATE", "1700000001 -0500"),
];

fn refused(out: Output, lacking: &str) {
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(128), "{stderr}");
    assert!(stderr.contains(lacking), "{stderr}");
    assert!(
        stderr.contains("alt config user.name \"Your Name\""),
        "{stderr}"
    );
    assert!(
        stderr.contains("alt config user.email \"you@example.com\""),
        "{stderr}"
    );
    assert!(
        !stderr.contains("localhost") && !stderr.contains("someone"),
        "{stderr}"
    );
}

#[test]
fn nobody_named_refuses_what_writes_somebody_and_changes_nothing() {
    for daemon in [false, true] {
        let dir = alt_os::temp::tempdir().unwrap();
        let root = dir.path();
        init(root);
        diverged(root, &|env, args| drop(ok(alt(root, daemon, env, args))));
        std::fs::write(root.join("staged"), "staged").unwrap();
        ok(alt(root, daemon, &[], &["add", "staged"]));
        ok(alt(root, daemon, &[], &["status"]));
        let before = state(root);

        let both = "name and email are not set";
        refused(
            alt(root, daemon, &[], &["commit", "-m", "nobody's"]),
            &format!("the committer's {both}"),
        );
        refused(
            alt(root, daemon, &[], &["commit", "--amend", "-m", "nobody's"]),
            both,
        );
        refused(
            alt(root, daemon, &[], &["tag", "-a", "marked", "-m", "marked"]),
            both,
        );
        let named = [("GIT_COMMITTER_NAME", "C"), ("GIT_COMMITTER_EMAIL", "c@x")];
        refused(
            commit(root, daemon, &named),
            "the author's name and email are not set",
        );
        let half = [("GIT_AUTHOR_NAME", "A"), ("EMAIL", "e@x")];
        refused(
            commit(root, daemon, &half),
            "the committer's name is not set",
        );
        let half = [
            ("GIT_AUTHOR_NAME", "A"),
            ("GIT_COMMITTER_NAME", "C"),
            ("GIT_COMMITTER_EMAIL", "c@x"),
        ];
        refused(commit(root, daemon, &half), "the author's email is not set");
        let unread = [("GIT_AUTHOR_DATE", "yesterday")];
        let all: Vec<_> = LATER.iter().chain(&unread).copied().collect();
        let out = commit(root, daemon, &all);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(128), "{stderr}");
        assert!(
            stderr.contains("invalid date format: yesterday"),
            "{stderr}"
        );
        assert_eq!(state(root), before, "daemon {daemon}");

        // nothing staged: the merge of two lines is to write a commit
        ok(alt(root, daemon, &LATER, &["commit", "-m", "staged"]));
        let before = state(root);
        refused(alt(root, daemon, &[], &["merge", "topic"]), both);
        refused(alt(root, daemon, &[], &["cherry-pick", "topic"]), both);
        refused(alt(root, daemon, &[], &["revert", "HEAD"]), both);
        assert_eq!(state(root), before, "daemon {daemon}");

        // what writes nobody goes on as before
        ok(alt(root, daemon, &[], &["status"]));
        ok(alt(root, daemon, &[], &["log"]));
        std::fs::write(root.join("more"), "more").unwrap();
        ok(alt(root, daemon, &[], &["add", "more"]));
        ok(alt(root, daemon, &[], &["tag", "light"]));
        ok(alt(root, daemon, &[], &["branch", "ahead"]));
        ok(alt(root, daemon, &LATER, &["commit", "-m", "more"]));
        ok(alt(root, daemon, &[], &["switch", "ahead"]));
        ok(alt(root, daemon, &[], &["merge", "main"]));
        let tip = |branch| ok(alt(root, daemon, &[], &["rev-parse", branch]));
        assert_eq!(tip("ahead"), tip("main"));
    }
}

#[test]
fn a_changed_configuration_names_the_next_commit() {
    for daemon in [false, true] {
        let dir = alt_os::temp::tempdir().unwrap();
        let root = dir.path();
        init(root);
        set(root, daemon, "user.name", "U");
        for address in ["one@x", "two@x", "three@x"] {
            set(root, daemon, "user.email", address);
            ok(commit(root, daemon, &[]));
            let by = format!("U <{address}>");
            let both = [format!("author {by}"), format!("committer {by}")];
            assert_eq!(who(&alt_lines(root, "HEAD")), both, "daemon {daemon}");
        }
        // a file written past `alt config` is read as well
        let config = root.join(".alt/git-import/config");
        let text = std::fs::read_to_string(&config).unwrap();
        std::fs::write(&config, text.replace("three@x", "four@x")).unwrap();
        ok(commit(root, daemon, &[]));
        let both = ["author U <four@x>", "committer U <four@x>"];
        assert_eq!(who(&alt_lines(root, "HEAD")), both, "daemon {daemon}");
    }
}
