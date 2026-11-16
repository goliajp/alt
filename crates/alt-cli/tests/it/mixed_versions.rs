//! One repository used by this build and by an older one that knows
//! nothing of the index runs, the head record or the anchored ref
//! snapshot: either may write, in turn or at once, and both read the same.
//!
//! The older program is whatever `ALT_OLD_BIN` names (an `alt` built from
//! the branch this work started from).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The older binary. These tests have nothing to say without one, so a
/// run that names none fails.
fn old_bin() -> PathBuf {
    let named = std::env::var_os("ALT_OLD_BIN")
        .expect("ALT_OLD_BIN names an alt built from the base branch");
    PathBuf::from(named)
}

fn run(bin: &Path, repo: &Path, args: &[&str]) -> Output {
    Command::new(bin)
        .current_dir(repo)
        .env("ALT_NO_DAEMON", "1")
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
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

fn new_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_alt"))
}

/// Writes `count` files named after `tag` and commits them with `bin`.
fn commit_files(bin: &Path, repo: &Path, tag: &str, count: usize) {
    for n in 0..count {
        fs::write(repo.join(format!("{tag}-{n}.txt")), format!("{tag} {n}\n")).unwrap();
    }
    ok(run(bin, repo, &["add", "."]));
    ok(run(
        bin,
        repo,
        &["commit", "-m", &format!("{tag}: {count} files")],
    ));
}

/// Both programs must give the same answer to every read.
fn same_reads(old: &Path, repo: &Path) {
    let new = new_bin();
    for args in [
        &["log"][..],
        &["status"],
        &["branch"],
        &["rev-parse", "HEAD"],
        &["cat-file", "-p", "HEAD"],
    ] {
        let by_old = ok(run(old, repo, args));
        let by_new = ok(run(&new, repo, args));
        assert_eq!(by_old, by_new, "alt {args:?}");
    }
    assert_eq!(head_tree(old, repo), head_tree(&new, repo));
    ok(run(&new, repo, &["fsck"]));
}

/// The entries of the tree of `HEAD`, as `bin` prints them.
fn head_tree(bin: &Path, repo: &Path) -> String {
    let commit = ok(run(bin, repo, &["cat-file", "-p", "HEAD"]));
    let tree = commit
        .lines()
        .next()
        .unwrap()
        .strip_prefix("tree ")
        .unwrap();
    ok(run(bin, repo, &["cat-file", "-p", tree]))
}

#[test]
#[ignore = "run with ALT_OLD_BIN set to an alt built from the base branch"]
fn old_and_new_take_turns_writing() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let repo = tmp.path();
    let (old, new) = (old_bin(), new_bin());
    ok(run(&new, repo, &["init"]));
    // enough for the new program to index and record
    commit_files(&new, repo, "new-a", 300);
    assert!(repo.join(".alt/map.alt.idx").is_dir());
    same_reads(&old, repo);

    // the old one appends past everything the new one indexed
    commit_files(&old, repo, "old-a", 300);
    same_reads(&old, repo);
    commit_files(&new, repo, "new-b", 300);
    same_reads(&old, repo);
    for round in 0..5 {
        commit_files(&old, repo, &format!("old-{round}"), 3);
        commit_files(&new, repo, &format!("new-{round}"), 3);
    }
    same_reads(&old, repo);
    assert_eq!(ok(run(&new, repo, &["log"])).lines().count(), 13);
    // every file either of them committed is in the tree both see
    assert_eq!(
        head_tree(&old, repo).lines().count(),
        930,
        "930 files committed"
    );
}

#[test]
#[ignore = "run with ALT_OLD_BIN set to an alt built from the base branch"]
fn old_and_new_write_at_the_same_time() {
    const ROUNDS: usize = 25;
    let tmp = alt_os::temp::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    fs::create_dir(&repo).unwrap();
    let (old, new) = (old_bin(), new_bin());
    ok(run(&new, &repo, &["init"]));
    commit_files(&new, &repo, "base", 300);
    // each program commits on a branch of its own, in a workspace of its own
    let sides: Vec<(PathBuf, PathBuf, &str)> =
        [(old.clone(), "old-side"), (new.clone(), "new-side")]
            .into_iter()
            .map(|(bin, name)| {
                let dir = tmp.path().join(name);
                ok(run(
                    &new,
                    &repo,
                    &["workspace", "add", name, dir.to_str().unwrap()],
                ));
                // a new workspace starts on a new branch named after it
                assert!(ok(run(&new, &dir, &["status"])).contains(name));
                (bin, dir, name)
            })
            .collect();

    let writers: Vec<_> = sides
        .iter()
        .cloned()
        .map(|(bin, dir, name)| {
            std::thread::spawn(move || {
                for round in 0..ROUNDS {
                    commit_files(&bin, &dir, &format!("{name}-{round}"), 12);
                }
            })
        })
        .collect();
    for writer in writers {
        writer.join().unwrap();
    }

    for (_, dir, name) in &sides {
        same_reads(&old, dir);
        let log = ok(run(&new, dir, &["log"]));
        assert_eq!(log.lines().count(), ROUNDS + 1, "{name}: {log}");
        assert_eq!(
            head_tree(&new, dir).lines().count(),
            300 + 12 * ROUNDS,
            "{name}"
        );
    }
    same_reads(&old, &repo);
}
