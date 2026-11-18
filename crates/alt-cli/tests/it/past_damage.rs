//! `ALT_READ_PAST_DAMAGE`: a command that only reads goes on past damaged
//! records, answers from what reads, says which bytes it passed over and
//! changes no file. Without it the same damage fails every command that
//! opens the damaged file.

use std::fs;
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::process::{Command, Output};

use super::unconfirmed::{alt, files, ok, stderr};

const PACK: &str = ".alt/store/pack-00000001.altpack";

/// `alt` reading past damage.
fn past(repo: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_alt"))
        .current_dir(repo)
        .env("ALT_NO_DAEMON", "1")
        .env("ALT_READ_PAST_DAMAGE", "1")
        .args(args)
        .output()
        .unwrap()
}

fn len(path: &Path) -> u64 {
    fs::metadata(path).unwrap().len()
}

/// Changes the byte at `at`, whatever it was.
fn flip_byte(path: &Path, at: u64) {
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    let mut byte = [0];
    file.read_exact_at(&mut byte, at).unwrap();
    file.write_all_at(&[!byte[0]], at).unwrap();
}

fn commit(dir: &Path, text: &str) -> String {
    fs::write(dir.join("a.txt"), text).unwrap();
    ok(alt(dir, &["add", "a.txt"]));
    ok(alt(dir, &["commit", "-m", text.trim()]));
    ok(alt(dir, &["rev-parse", "HEAD"])).trim().to_owned()
}

/// A repository with two commits, then a blob no commit names — its
/// records start where the pack and `map.alt` ended before it —, then a
/// third commit. Returns those two lengths and the id of the last commit.
fn repo(dir: &Path) -> (u64, u64, String) {
    ok(alt(dir, &["init"]));
    commit(dir, "one\n");
    commit(dir, "two\n");
    let before = (len(&dir.join(PACK)), len(&dir.join(".alt/map.alt")));
    fs::write(dir.join("spare.txt"), "in no commit\n").unwrap();
    ok(alt(dir, &["add", "spare.txt"]));
    fs::remove_file(dir.join("spare.txt")).unwrap();
    ok(alt(dir, &["add", "spare.txt"]));
    let last = commit(dir, "three\n");
    (before.0, before.1, last)
}

/// With the damage in place: `args` fails saying `message`; reading past
/// the damage it succeeds, names the bytes `from..to` of `file`, and no
/// file under `.alt` changes. Returns what it printed.
fn reads_past(dir: &Path, args: &[&str], message: &str, passed: (&str, u64, u64)) -> String {
    let failed = alt(dir, args);
    assert!(!failed.status.success(), "{args:?} met no damage");
    assert!(stderr(&failed).contains(message), "{}", stderr(&failed));

    let before = files(&dir.join(".alt"));
    let out = past(dir, args);
    let (file, from, to) = passed;
    let warning = format!("warning: passed over bytes {from} to {to} of ");
    let said = stderr(&out);
    assert!(said.contains(&warning) && said.contains(file), "{said}");
    assert!(
        files(&dir.join(".alt")) == before,
        "{args:?} changed a file"
    );
    ok(out)
}

#[test]
fn records_after_a_damaged_one_in_map_alt_are_read() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    let (_, spare, last) = repo(dir);
    // inside the record of the blob no commit names
    flip_byte(&dir.join(".alt/map.alt"), spare + 40);

    let passed = ("map.alt", spare, spare + 82);
    let shown = reads_past(
        dir,
        &["cat-file", "-p", &last],
        "map.alt record corrupt",
        passed,
    );
    assert!(shown.ends_with("three\n"), "{shown}");
    let log = reads_past(dir, &["log", "-n", "5"], "map.alt record corrupt", passed);
    assert_eq!(log.lines().count(), 3, "{log}");

    // every commit is whole: all of it leaves for git
    let target = tmp.path().join("exported");
    let before = files(&dir.join(".alt"));
    let export = past(dir, &["export", target.to_str().unwrap()]);
    assert!(stderr(&export).contains("passed over bytes"), "{export:?}");
    assert!(ok(export).contains("exported"));
    assert!(target.join(".git").is_dir());
    assert!(files(&dir.join(".alt")) == before);
}

#[test]
fn records_after_a_damaged_one_in_the_pack_are_read() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    let (spare, _, last) = repo(dir);
    // an encoding no record has: the walk of the pack stops at this header
    flip_byte(&dir.join(PACK), spare + 32);

    let out = past(dir, &["cat-file", "-p", &last]);
    let said = stderr(&out);
    let warning = format!("warning: passed over bytes {spare} to ");
    assert!(
        said.contains(&warning) && said.contains("pack-00000001"),
        "{said}"
    );
    assert!(ok(out).ends_with("three\n"));
    let failed = alt(dir, &["cat-file", "-p", &last]);
    assert!(
        stderr(&failed).contains("active pack damaged"),
        "{failed:?}"
    );
}

#[test]
fn the_log_is_read_up_to_a_damaged_operation() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    let (_, _, last) = repo(dir);
    let log = dir.join(".alt/oplog/log");
    let before = len(&log);
    ok(alt(dir, &["branch", "later"]));
    flip_byte(&log, before + 30);

    let passed = ("oplog/log", before, len(&log));
    let head = reads_past(dir, &["rev-parse", "HEAD"], "oplog record corrupt", passed);
    assert_eq!(head.trim(), last);
    let branches = reads_past(dir, &["branch"], "oplog record corrupt", passed);
    assert!(!branches.contains("later"), "{branches}");
}

#[test]
fn a_command_that_writes_and_the_full_check_do_not_read_past_damage() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    repo(dir);
    let before = files(&dir.join(".alt"));
    fs::write(dir.join("b.txt"), "b\n").unwrap();
    for args in [&["add", "b.txt"][..], &["commit", "-m", "x"], &["fsck"]] {
        let out = past(dir, args);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {out:?}");
        assert!(stderr(&out).contains("ALT_READ_PAST_DAMAGE"), "{out:?}");
    }
    assert!(files(&dir.join(".alt")) == before);
}

/// A process that reads past damage opens the store to be read and
/// nothing else: like any command that only reads it leaves the record of
/// the store's files in `odb.lock` to the next write, and unlike them it
/// leaves the snapshot of the refs too.
#[test]
fn reading_past_damage_leaves_what_an_open_would_bring_up_to_date() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    let (_, _, last) = repo(dir);
    fs::write(dir.join(".alt/odb.lock"), b"").unwrap();
    let before = files(&dir.join(".alt"));
    ok(past(dir, &["cat-file", "-p", &last]));
    assert!(files(&dir.join(".alt")) == before);
    ok(alt(dir, &["cat-file", "-p", &last]));
    assert!(files(&dir.join(".alt")) == before);
    ok(alt(dir, &["tag", "-a", "written", "-m", "written"]));
    let record = fs::read(dir.join(".alt/odb.lock")).unwrap();
    assert!(record.starts_with(b"ALTH"), "nothing to bring up to date");

    // the same for the snapshot of the refs, which an open writes once
    // the log has grown enough past it
    for n in 0..40 {
        ok(alt(dir, &["branch", &format!("b{n}")]));
    }
    let snapshot = dir.join(".alt/refs/snapshot");
    fs::remove_file(&snapshot).unwrap();
    ok(past(dir, &["rev-parse", "HEAD"]));
    assert!(!snapshot.exists());
    ok(alt(dir, &["rev-parse", "HEAD"]));
    assert!(snapshot.exists(), "nothing to bring up to date");
}

#[test]
fn the_daemon_does_not_start_reading_past_damage() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    repo(dir);
    let out = Command::new(env!("CARGO_BIN_EXE_altd"))
        .arg(dir.join(".alt"))
        .env("ALT_READ_PAST_DAMAGE", "1")
        .env("ALT_DAEMON_IDLE_MS", "300")
        .output()
        .unwrap();
    assert!(!out.status.success(), "{out:?}");
    assert!(stderr(&out).contains("ALT_READ_PAST_DAMAGE"), "{out:?}");
}

#[test]
fn an_export_fails_on_a_ref_whose_object_is_not_stored_and_past_damage_leaves_the_ref_out() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = &tmp.path().join("repo");
    fs::create_dir(dir).unwrap();
    let second = super::fsck_tails::store_behind_its_log(dir);
    let before = files(&dir.join(".alt"));

    let target = tmp.path().join("exported");
    let export = alt(dir, &["export", target.to_str().unwrap()]);
    assert!(!export.status.success(), "{export:?}");
    let said = stderr(&export);
    assert!(
        said.contains("which is not stored; nothing was exported"),
        "{said}"
    );
    assert!(said.contains("ref refs/"), "{said}");
    assert!(!target.join(".git").exists());

    let export = past(dir, &["export", target.to_str().unwrap()]);
    let said = stderr(&export);
    let left = format!(
        "warning: left out ref refs/heads/main: it points at {second}, which is not stored"
    );
    assert!(said.contains(&left), "{said}");
    assert!(ok(export).contains("exported"));
    let git = |args: &[&str]| {
        Command::new("git")
            .current_dir(&target)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .args(args)
            .output()
            .unwrap()
    };
    let fsck = git(&["fsck"]);
    assert!(fsck.status.success(), "{fsck:?}");
    let refs = String::from_utf8(git(&["for-each-ref"]).stdout).unwrap();
    assert!(!refs.contains("refs/heads/main"), "{refs}");
    assert_eq!(files(&dir.join(".alt")), before);
}
