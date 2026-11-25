//! A `mv` of a directory takes the empty directories in it along, which
//! the older program knows nothing of. What the move leaves in the log
//! is of its files alone: the older program finishes it and takes it
//! back as a move of those files, writes no file where a directory is,
//! and goes on writing to the store.

use std::path::Path;
use std::process::Output;

use super::step_mark::crashing;
use super::{new_bin, ok, old_bin, run, same_reads};

/// `d` with a file and two directories that hold nothing, committed.
fn with_hollow(repo: &Path) {
    let new = new_bin();
    ok(run(&new, repo, &["init", "."]));
    std::fs::create_dir_all(repo.join("d/empty")).unwrap();
    std::fs::create_dir_all(repo.join("d/s/hollow")).unwrap();
    std::fs::write(repo.join("d/f"), "1\n").unwrap();
    ok(run(&new, repo, &["add", "."]));
    ok(run(&new, repo, &["commit", "-m", "c1"]));
}

/// Every name beneath `dir` but those of the store, with what it is.
fn names(dir: &Path) -> Vec<(String, &'static str)> {
    let mut found = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(at) = stack.pop() {
        for entry in std::fs::read_dir(&at).unwrap() {
            let entry = entry.unwrap();
            if entry.file_name() == ".alt" {
                continue;
            }
            let kind = match entry.file_type().unwrap() {
                kind if kind.is_dir() => "directory",
                kind if kind.is_file() => "file",
                _ => "other",
            };
            let path = entry.path();
            let name = path.strip_prefix(dir).unwrap().to_str().unwrap().to_owned();
            if kind == "directory" {
                stack.push(path);
            }
            found.push((name, kind));
        }
    }
    found.sort();
    found
}

fn said(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

const MOVED: [(&str, &str); 5] = [
    ("e", "directory"),
    ("e/empty", "directory"),
    ("e/f", "file"),
    ("e/s", "directory"),
    ("e/s/hollow", "directory"),
];

fn moved() -> Vec<(String, &'static str)> {
    MOVED.iter().map(|(n, k)| (n.to_string(), *k)).collect()
}

/// The file is at `at` alone, and each of the two empty directories is
/// a directory still, in the one place or the other.
fn assert_whole(repo: &Path, at: &str, when: &str) {
    let found = names(repo);
    let files: Vec<&str> = found
        .iter()
        .filter(|f| f.1 == "file")
        .map(|f| &*f.0)
        .collect();
    assert_eq!(files, [at], "{when}: {found:?}");
    for name in ["empty", "s/hollow"] {
        let held = |f: &&(String, &str)| f.0 == format!("d/{name}") || f.0 == format!("e/{name}");
        let held: Vec<_> = found.iter().filter(held).collect();
        assert_eq!(held.len(), 1, "{when}: {found:?}");
        assert_eq!(held[0].1, "directory", "{when}: {found:?}");
    }
}

/// What is kept of moved directories beside the index.
fn kept(repo: &Path) -> usize {
    match std::fs::read_dir(repo.join(".alt/moved-empty")) {
        Ok(kept) => kept.count(),
        Err(_) => 0,
    }
}

/// The points a `mv d e` passes once its intent is in the log.
fn points_after_the_intent() -> std::ops::RangeInclusive<usize> {
    let listed = alt_os::temp::tempdir().unwrap();
    with_hollow(listed.path());
    let list = listed.path().join(".alt/points");
    let whole = crashing(
        listed.path(),
        &["mv", "d", "e"],
        &format!("list:{}", list.display()),
    );
    assert!(whole.status.success(), "{}", said(&whole));
    let points = std::fs::read_to_string(&list).unwrap();
    let points: Vec<&str> = points.lines().collect();
    let recorded = points.iter().position(|p| *p == "checkout-intent:after");
    let recorded = recorded.expect("the intent of the move is recorded");
    assert!(points.len() > recorded + 1, "{points:?}");
    recorded + 1..=points.len()
}

/// `mv d e` killed at the point `at`; the operation left to finish.
fn killed(repo: &Path, at: usize) -> String {
    with_hollow(repo);
    let killed = crashing(repo, &["mv", "d", "e"], &at.to_string());
    assert_eq!(killed.status.code(), None, "point {at}");
    let pending = ok(run(&new_bin(), repo, &["operation", "pending"]));
    assert_eq!(pending.lines().count(), 1, "point {at}: {pending}");
    pending.split(' ').next().unwrap().to_owned()
}

#[test]
#[ignore = "run with ALT_OLD_BIN set to an alt built from the base branch"]
fn the_older_program_finishes_a_move_that_takes_empty_directories_as_one_of_files() {
    let (old, new) = (old_bin(), new_bin());
    for at in points_after_the_intent() {
        let when = format!("point {at}");
        let dir = alt_os::temp::tempdir().unwrap();
        let repo = dir.path();
        let id = killed(repo, at);
        ok(run(&old, repo, &["operation", "recover", &id]));
        assert_whole(repo, "e/f", &when);
        for bin in [&old, &new] {
            assert_eq!(ok(run(bin, repo, &["operation", "pending"])), "", "{when}");
        }
        // and writes on
        ok(run(&old, repo, &["branch", "kept"]));
        std::fs::write(repo.join("e/f"), "by the older program\n").unwrap();
        ok(run(&old, repo, &["add", "."]));
        ok(run(&old, repo, &["commit", "-m", "moved"]));
        same_reads(&old, repo);
        assert_whole(repo, "e/f", &when);

        // this program finishes it whole
        let dir = alt_os::temp::tempdir().unwrap();
        let repo = dir.path();
        let id = killed(repo, at);
        ok(run(&new, repo, &["operation", "recover", &id]));
        assert_eq!(ok(run(&new, repo, &["operation", "pending"])), "", "{when}");
        assert_eq!(names(repo), moved(), "{when}");
        ok(run(&new, repo, &["fsck"]));
    }
}

#[test]
#[ignore = "run with ALT_OLD_BIN set to an alt built from the base branch"]
fn the_older_program_takes_back_a_move_that_took_empty_directories_as_one_of_files() {
    let (old, new) = (old_bin(), new_bin());
    let dir = alt_os::temp::tempdir().unwrap();
    let repo = dir.path();
    with_hollow(repo);
    let status = || ok(run(&new, repo, &["status", "--json"]));
    let before = status();
    ok(run(&new, repo, &["mv", "d", "e"]));
    assert_eq!(names(repo), moved());
    assert_eq!(kept(repo), 1);

    ok(run(&old, repo, &["undo"]));
    assert_whole(repo, "d/f", "taken back by the older program");
    assert_eq!(status(), before);
    // nothing in the log keeps the older program from writing
    ok(run(&old, repo, &["branch", "kept"]));
    same_reads(&old, repo);

    // this program takes its own move back whole, and makes it again
    let dir = alt_os::temp::tempdir().unwrap();
    let repo = dir.path();
    with_hollow(repo);
    ok(run(&new, repo, &["mv", "d", "e"]));
    ok(run(&new, repo, &["undo"]));
    assert!(repo.join("d/empty").is_dir() && repo.join("d/s/hollow").is_dir());
    assert!(repo.join("d/f").is_file() && !repo.join("e").exists());
    assert_eq!(kept(repo), 1);
    ok(run(&new, repo, &["undo"]));
    assert_eq!(names(repo), moved());
    // what was kept goes once a later change of files is made
    ok(run(&new, repo, &["mv", "e/f", "e/g"]));
    assert_eq!(kept(repo), 0);
    ok(run(&new, repo, &["fsck"]));
}
