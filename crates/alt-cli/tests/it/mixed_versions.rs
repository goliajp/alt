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

/// The version byte of the ref snapshot, where there is one.
fn snapshot_version(repo: &Path) -> Option<u8> {
    let snapshot = fs::read(repo.join(".alt/refs/snapshot")).ok()?;
    assert_eq!(&snapshot[..4], b"ALTS");
    Some(snapshot[4])
}

/// The ref snapshot is a cache each program writes in its own version:
/// this build's carries a note of how far the log is synced, which the
/// older one does not know and takes for no snapshot. Whichever read or
/// wrote last, both answer the same, and once this build has read, the
/// snapshot is one of its own again.
#[test]
#[ignore = "run with ALT_OLD_BIN set to an alt built from the base branch"]
fn the_ref_snapshot_changes_hands_and_both_programs_read_the_same() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let repo = tmp.path();
    let (old, new) = (old_bin(), new_bin());
    ok(run(&new, repo, &["init"]));
    // enough operations for a snapshot to be due
    commit_files(&new, repo, "new-a", 300);
    let head = ok(run(&new, repo, &["rev-parse", "HEAD"]));
    assert_eq!(snapshot_version(repo), Some(3));

    for round in 0..3 {
        // the older program reads past a snapshot it does not know
        assert_eq!(ok(run(&old, repo, &["rev-parse", "HEAD"])), head, "{round}");
        let by_old = snapshot_version(repo);
        same_reads(&old, repo);
        // and this build reads whatever the older one left there
        assert_eq!(ok(run(&new, repo, &["rev-parse", "HEAD"])), head, "{round}");
        assert_eq!(snapshot_version(repo), Some(3), "{round}: after {by_old:?}");
        ok(run(&new, repo, &["fsck"]));
    }

    // either writes on top of a snapshot the other left
    commit_files(&old, repo, "old-a", 300);
    let by_old = snapshot_version(repo);
    same_reads(&old, repo);
    assert_eq!(snapshot_version(repo), Some(3), "after {by_old:?}");
    commit_files(&new, repo, "new-b", 300);
    assert_eq!(snapshot_version(repo), Some(3));
    same_reads(&old, repo);
    commit_files(&old, repo, "old-b", 3);
    same_reads(&old, repo);
    assert_eq!(ok(run(&new, repo, &["log"])).lines().count(), 4);
    assert_eq!(head_tree(&old, repo).lines().count(), 903);
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

#[test]
#[ignore = "run with ALT_OLD_BIN set to an alt built from the base branch"]
fn what_the_old_program_left_unfinished_does_not_keep_the_new_one_out() {
    // (file with part of a record at its end, markers removed)
    let cases = [
        (None, false),
        (Some("map.alt"), false),
        (Some("oplog/log"), false),
        (Some("store/pack-00000001.altpack"), false),
        (None, true),
        (Some("map.alt"), true),
        (Some("oplog/log"), true),
    ];
    for (torn, unmarked) in cases {
        let case = format!("{torn:?}, markers removed: {unmarked}");
        let tmp = alt_os::temp::tempdir().unwrap();
        let repo = tmp.path();
        let (old, new) = (old_bin(), new_bin());
        ok(run(&old, repo, &["init"]));
        for round in 0..3 {
            commit_files(&old, repo, &format!("old-{round}"), 3);
        }
        let alt_dir = repo.join(".alt");
        if unmarked {
            fs::remove_file(alt_dir.join("odb.durable")).unwrap();
            fs::remove_file(alt_dir.join("oplog/durable")).unwrap();
        }
        if let Some(file) = torn {
            let mut data = fs::read(alt_dir.join(file)).unwrap();
            data.extend_from_slice(b"half a record");
            fs::write(alt_dir.join(file), data).unwrap();
        }
        for args in [&["status"][..], &["log", "-n", "1"], &["fsck"]] {
            let out = run(&new, repo, args);
            assert!(out.status.success(), "{case}: alt {args:?}");
        }
        commit_files(&new, repo, "new", 3);
        assert_eq!(ok(run(&new, repo, &["log"])).lines().count(), 4, "{case}");
        // and the old one goes on with what the new one left
        same_reads(&old, repo);
        commit_files(&old, repo, "old-again", 3);
        commit_files(&new, repo, "new-again", 3);
        same_reads(&old, repo);
        assert_eq!(head_tree(&new, repo).lines().count(), 18, "{case}");
    }
}

/// The one operation `bin` lists as waiting for a recovery.
fn pending(bin: &Path, repo: &Path) -> String {
    let listed = ok(run(bin, repo, &["operation", "pending"]));
    listed.split_whitespace().next().unwrap().to_owned()
}

/// `bin` refuses to write, asking for a recovery of `op`, and changes
/// neither the index nor what `HEAD` names.
fn refuses_to_write(bin: &Path, repo: &Path, op: &str) {
    let index = fs::read(repo.join(".alt/index")).unwrap();
    let head = ok(run(bin, repo, &["rev-parse", "HEAD"]));
    fs::write(repo.join("refused.txt"), "refused\n").unwrap();
    for args in [&["add", "refused.txt"][..], &["commit", "-m", "refused"]] {
        let out = run(bin, repo, args);
        let said = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "alt {args:?}");
        let asked = format!("operation {op} has no durable completion; recovery required");
        assert!(said.contains(&asked), "alt {args:?}: {said}");
    }
    fs::remove_file(repo.join("refused.txt")).unwrap();
    assert_eq!(fs::read(repo.join(".alt/index")).unwrap(), index);
    assert_eq!(ok(run(bin, repo, &["rev-parse", "HEAD"])), head);
}

/// This build closes an add it cannot complete with a record the older
/// program does not know. The older one reads the repository as this
/// build does and takes the add for unfinished: it refuses to write, and
/// recovers the add only onto the index from before it, which this build
/// then reports and puts right. Nothing either of them stored is lost.
#[test]
#[ignore = "run with ALT_OLD_BIN set to an alt built from the base branch"]
fn the_older_program_takes_an_abandoned_add_for_unfinished_and_damages_nothing() {
    let (old, new) = (old_bin(), new_bin());

    // abandoned by a command that leaves the index as before the add
    let tmp = alt_os::temp::tempdir().unwrap();
    let repo = tmp.path();
    super::abandoned_add::add_cut_short(repo, false);
    let op = pending(&new, repo);
    ok(run(&new, repo, &["tag", "v1"]));
    assert_eq!(ok(run(&new, repo, &["operation", "pending"])), "");
    same_reads(&old, repo);
    assert_eq!(pending(&old, repo), op);
    refuses_to_write(&old, repo, &op);
    // the older recovery applies the add: the index names the lost blob
    ok(run(&old, repo, &["operation", "recover", &op]));
    assert_eq!(ok(run(&old, repo, &["operation", "pending"])), "");
    let head = ok(run(&old, repo, &["rev-parse", "HEAD"]));
    assert!(!run(&old, repo, &["commit", "-m", "lost"]).status.success());
    assert_eq!(ok(run(&new, repo, &["rev-parse", "HEAD"])), head);
    let checked = run(&new, repo, &["fsck"]);
    let said = String::from_utf8_lossy(&checked.stderr);
    assert!(!checked.status.success());
    assert!(said.contains("index entry new.txt names blob "), "{said}");
    assert_eq!(ok(run(&new, repo, &["operation", "pending"])), "");
    ok(run(&new, repo, &["add", "new.txt"]));
    ok(run(&new, repo, &["commit", "-m", "second"]));
    same_reads(&old, repo);
    commit_files(&old, repo, "old", 3);
    commit_files(&new, repo, "new", 3);
    same_reads(&old, repo);
    assert_eq!(head_tree(&new, repo).lines().count(), 8);

    // abandoned, and this build has changed the index since
    let tmp = alt_os::temp::tempdir().unwrap();
    let repo = tmp.path();
    super::abandoned_add::add_cut_short(repo, false);
    let op = pending(&new, repo);
    commit_files(&new, repo, "new-a", 3);
    same_reads(&old, repo);
    refuses_to_write(&old, repo, &op);
    let recovery = run(&old, repo, &["operation", "recover", &op]);
    let said = String::from_utf8_lossy(&recovery.stderr);
    assert!(!recovery.status.success());
    assert!(
        said.contains("index matches neither recorded state"),
        "{said}"
    );
    refuses_to_write(&old, repo, &op);
    same_reads(&old, repo);
    commit_files(&new, repo, "new-b", 3);
    same_reads(&old, repo);
    assert_eq!(head_tree(&new, repo).lines().count(), 8);
}
