//! A merge killed while it writes the conflict it stops at: the write is in
//! the log as a checkout is, with all it needs stored, and is finished
//! from there.

use std::path::Path;
use std::process::Output;

fn alt(repo: &Path, args: &[&str], crash: Option<&str>) -> Output {
    let mut command = crate::alt_command();
    command
        .current_dir(repo)
        .env("ALT_NO_DAEMON", "1")
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .args(args);
    if let Some(crash) = crash {
        command.env("ALT_CRASH_POINT", crash);
    }
    command.output().unwrap()
}

fn ok(repo: &Path, args: &[&str]) -> String {
    let done = alt(repo, args, None);
    let said = String::from_utf8_lossy(&done.stderr);
    assert!(done.status.success(), "{args:?}: {said}");
    String::from_utf8(done.stdout).unwrap()
}

/// `main` and `side` both change `f.txt`; `side` alone changes `t.txt` and
/// adds `new.txt`.
fn diverged(repo: &Path) {
    let commit = |files: &[(&str, &str)], message: &str| {
        for (file, text) in files {
            std::fs::write(repo.join(file), text).unwrap();
        }
        ok(repo, &["add", "."]);
        ok(repo, &["commit", "-m", message]);
    };
    ok(repo, &["init", "."]);
    commit(&[("f.txt", "1\n2\n3\n"), ("t.txt", "t\n")], "files");
    ok(repo, &["switch", "-c", "side"]);
    let by_side = [
        ("f.txt", "1\nside\n3\n"),
        ("t.txt", "side\n"),
        ("new.txt", "new\n"),
    ];
    commit(&by_side, "side");
    ok(repo, &["switch", "main"]);
    commit(&[("f.txt", "1\nmain\n3\n")], "main");
}

#[test]
fn a_conflict_killed_while_it_is_written_is_finished_from_the_log() {
    let dirs = [
        alt_os::temp::tempdir().unwrap(),
        alt_os::temp::tempdir().unwrap(),
    ];
    let [listed, repo] = [dirs[0].path(), dirs[1].path()];
    diverged(listed);
    diverged(repo);
    let list = listed.join(".alt/points");
    let whole = alt(
        listed,
        &["merge", "side"],
        Some(&format!("list:{}", list.display())),
    );
    assert_eq!(whole.status.code(), Some(1));
    let points = std::fs::read_to_string(list).unwrap();
    let writes = |(at, point): (usize, &str)| (point == "checkout-file:before").then_some(at + 1);
    let writes: Vec<usize> = points.lines().enumerate().filter_map(writes).collect();
    // the three files the merge changes, each written as a checkout writes it
    assert_eq!(writes.len(), 3, "{points}");

    // one file is written, two and the index are not
    let killed = alt(repo, &["merge", "side"], Some(&writes[1].to_string()));
    assert_eq!(killed.status.code(), None);
    let status = ok(repo, &["status", "--json"]);
    assert!(!status.contains(r#""unmerged":["f.txt"]"#), "{status}");

    let pending = ok(repo, &["operation", "pending"]);
    let intent = pending.lines().last().expect("an operation to finish");
    let id = intent.split(' ').next().unwrap();
    ok(repo, &["operation", "recover", id]);
    assert_eq!(ok(repo, &["operation", "pending"]), "");
    let status = ok(repo, &["status", "--json"]);
    assert!(status.contains(r#""unmerged":["f.txt"]"#), "{status}");
    let read = |file: &str| std::fs::read_to_string(repo.join(file)).unwrap();
    assert!(
        read("f.txt").contains("<<<<<<< HEAD\nmain\n"),
        "{}",
        read("f.txt")
    );
    assert_eq!(
        (read("t.txt"), read("new.txt")),
        ("side\n".into(), "new\n".into())
    );
    ok(repo, &["fsck"]);
    assert_eq!(
        read("f.txt"),
        std::fs::read_to_string(listed.join("f.txt")).unwrap()
    );
}
