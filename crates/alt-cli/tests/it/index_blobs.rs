//! An index that names a blob the store does not hold: the index is
//! written with its own fsync, so after a power cut it can be ahead of
//! the object store. The check reports the entry, a commit refuses to
//! build a tree on it, and adding the file again stores the blob.
//!
//! The intent of an add is in the operation log before the blobs are
//! synced. One a power cut left there without its blobs was never
//! confirmed: readers pass it over and the next writer cuts it, with all
//! that follows, as they do a ref transaction whose objects are lost.

use std::fs;
use std::path::Path;

use super::crash_fixture::remove_final_log_frame;
use super::unconfirmed::{Lost, alt, alt_command, ok, power_cut, stderr};
use super::unconfirmed_tails::{LOGS, MARKERS, lag, lens};

pub(super) fn stdout(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Runs `stage` and then takes the object store back to where it was
/// before, markers and all: the objects `stage` stored never arrived.
/// The operation log and the index keep what `stage` wrote.
fn lose_objects_of(dir: &Path, stage: impl FnOnce()) {
    let before = lens(dir);
    let markers = MARKERS.map(|marker| fs::read(dir.join(".alt").join(marker)).unwrap());
    let log = fs::read(dir.join(".alt/oplog/durable")).unwrap();
    stage();
    for (file, len) in LOGS.iter().zip(before).take(4) {
        let file = fs::OpenOptions::new()
            .write(true)
            .open(dir.join(".alt").join(file))
            .unwrap();
        file.set_len(len).unwrap();
    }
    let now = fs::read(dir.join(".alt/oplog/durable")).unwrap();
    lag(dir, &markers);
    // the operation log was synced: only the object store is behind
    assert_ne!(now, log);
    fs::write(dir.join(".alt/oplog/durable"), now).unwrap();
}

/// One commit, then `new.txt` staged and its blob lost.
fn staged_and_lost(dir: &Path) -> String {
    ok(alt(dir, &["init"]));
    fs::write(dir.join("a.txt"), "one\n").unwrap();
    ok(alt(dir, &["add", "a.txt"]));
    ok(alt(dir, &["commit", "-m", "first"]));
    let head = ok(alt(dir, &["rev-parse", "HEAD"]));
    fs::write(dir.join("new.txt"), "never arrived\n").unwrap();
    lose_objects_of(dir, || drop(ok(alt(dir, &["add", "new.txt"]))));
    head
}

#[test]
fn a_commit_refuses_an_index_entry_whose_blob_is_not_stored() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    let head = staged_and_lost(dir);
    let commit = alt(dir, &["commit", "-m", "second"]);
    assert!(!commit.status.success(), "{}", stdout(&commit));
    let said = stderr(&commit);
    assert!(
        said.contains("index entry new.txt names blob ")
            && said.contains(", which is not stored: add the file again"),
        "{said}"
    );
    assert_eq!(ok(alt(dir, &["rev-parse", "HEAD"])), head);
    // a file that was committed before is not looked up again
    ok(alt(dir, &["add", "new.txt"]));
    ok(alt(dir, &["commit", "-m", "second"]));
    assert_ne!(ok(alt(dir, &["rev-parse", "HEAD"])), head);
    ok(alt(dir, &["fsck"]));
}

#[test]
fn an_amended_commit_refuses_it_too() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    let head = staged_and_lost(dir);
    let amend = alt(dir, &["commit", "--amend", "-m", "first, again"]);
    assert!(!amend.status.success());
    assert!(stderr(&amend).contains("index entry new.txt names blob "));
    assert_eq!(ok(alt(dir, &["rev-parse", "HEAD"])), head);
}

#[test]
fn the_check_reports_the_entries_of_every_workspace() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path().join("main");
    fs::create_dir(&dir).unwrap();
    let other = tmp.path().join("other");
    ok(alt(&dir, &["init"]));
    fs::write(dir.join("a.txt"), "one\n").unwrap();
    ok(alt(&dir, &["add", "a.txt"]));
    ok(alt(&dir, &["commit", "-m", "first"]));
    ok(alt(&dir, &["branch", "feat"]));
    let tree = other.to_str().unwrap();
    ok(alt(&dir, &["workspace", "add", "other", tree, "feat"]));
    ok(alt(&dir, &["fsck"]));
    lose_objects_of(&dir, || {
        fs::write(dir.join("here.txt"), "in the first workspace\n").unwrap();
        ok(alt(&dir, &["add", "here.txt"]));
        fs::write(other.join("there.txt"), "in the second\n").unwrap();
        ok(alt(&other, &["add", "there.txt"]));
    });
    let check = alt(&dir, &["fsck"]);
    assert!(!check.status.success());
    let said = stderr(&check);
    assert!(said.contains("index entry here.txt names blob "), "{said}");
    assert!(
        said.contains("index entry there.txt of workspace other names blob "),
        "{said}"
    );
    assert!(!said.contains("a.txt"), "{said}");
}

/// An add that was interrupted before its completion was recorded, with
/// the blob it stored lost as well: recovery does not complete it. It
/// abandons it, and puts back the index from before the add.
#[test]
fn recovery_does_not_complete_an_add_whose_blob_is_not_stored() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    staged_and_lost(dir);
    remove_final_log_frame(dir);
    let pending: alt_json::Value =
        alt_json::from_str(&ok(alt(dir, &["operation", "pending", "--json"]))).unwrap();
    let id = pending["operations"][0]["operation_id"].as_str().unwrap();
    let recover = alt(dir, &["operation", "recover", id]);
    let said = stderr(&recover);
    assert!(recover.status.success(), "{said}");
    assert!(
        said.contains(&format!("warning: abandoned operation {id}: ")),
        "{said}"
    );
    assert_eq!(stdout(&recover), format!("{id} abandoned\n"));
    let index = fs::read(dir.join(".alt/index")).unwrap();
    assert!(!index.windows(7).any(|at| at == b"new.txt"));
    assert_eq!(ok(alt(dir, &["operation", "pending"])), "");
    ok(alt(dir, &["add", "new.txt"]));
    ok(alt(dir, &["commit", "-m", "second"]));
    ok(alt(dir, &["fsck"]));
}

/// The operation log without its last record; the marker stays.
pub(super) fn drop_last_op(dir: &Path) {
    let path = dir.join(".alt/oplog/log");
    let bytes = fs::read(&path).unwrap();
    let (mut at, mut last) = (5, 5);
    while at < bytes.len() {
        last = at;
        at += 4 + u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize + 8;
    }
    fs::write(path, &bytes[..last]).unwrap();
}

/// One commit, then `new.txt` added and whatever `then` does, and the
/// state a power cut leaves when none of it was synced: the operation
/// log holds it, the object store and both markers are as before the add.
/// Returns the index as it was before the add.
fn add_never_synced(dir: &Path, then: impl FnOnce()) -> Vec<u8> {
    ok(alt(dir, &["init"]));
    fs::write(dir.join("a.txt"), "one\n").unwrap();
    ok(alt(dir, &["add", "a.txt"]));
    ok(alt(dir, &["commit", "-m", "first"]));
    let before = lens(dir);
    let markers = MARKERS.map(|marker| fs::read(dir.join(".alt").join(marker)).unwrap());
    let index = fs::read(dir.join(".alt/index")).unwrap();
    fs::write(dir.join("new.txt"), "never arrived\n").unwrap();
    ok(alt(dir, &["add", "new.txt"]));
    then();
    for (file, len) in LOGS.iter().zip(before).take(4) {
        let file = fs::OpenOptions::new()
            .write(true)
            .open(dir.join(".alt").join(file))
            .unwrap();
        file.set_len(len).unwrap();
    }
    lag(dir, &markers);
    index
}

fn left_out(out: &std::process::Output, ops: usize) -> bool {
    stderr(out).contains(&format!(
        "warning: the last {ops} operation(s) in the operation log were never synced"
    ))
}

/// The cut came between the intent and the sync that goes before the
/// index is replaced: the intent is in the log, the blob is not stored,
/// the index is the old one. No recovery is asked for.
#[test]
fn an_add_cut_before_its_sync_is_passed_over_and_discarded() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    let index = add_never_synced(dir, || ());
    // neither the index nor the completion was written
    drop_last_op(dir);
    crate::replace_file(&dir.join(".alt/index"), &index);
    let check = alt(dir, &["fsck"]);
    assert_eq!(check.status.code(), Some(3), "{}", stderr(&check));
    let said = stdout(&check);
    assert!(
        said.contains("1 operation(s) at the end of the operation log")
            && said.contains("1 operation(s) that move no ref"),
        "{said}"
    );
    let pending = alt(dir, &["operation", "pending", "--json"]);
    assert!(left_out(&pending, 1), "{}", stderr(&pending));
    let listed: alt_json::Value = alt_json::from_str(&ok(pending)).unwrap();
    assert_eq!(listed["operations"].as_array().unwrap().len(), 0);
    let status = alt(dir, &["status"]);
    assert!(status.status.success() && left_out(&status, 1));
    assert_eq!(fs::read(dir.join(".alt/index")).unwrap(), index);

    let add = alt(dir, &["add", "new.txt"]);
    let said = stderr(&add);
    assert!(add.status.success(), "{said}");
    assert!(
        said.contains("warning: discarded 1 operation(s) at the end of the operation log"),
        "{said}"
    );
    ok(alt(dir, &["commit", "-m", "second"]));
    ok(alt(dir, &["fsck"]));
}

/// What was appended after such an intent was never confirmed either:
/// every command that reads answers without it, an export writes the
/// repository out without it, and the next writer cuts all of it.
#[test]
fn what_follows_an_add_that_was_never_synced_goes_with_it() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = &tmp.path().join("repo");
    fs::create_dir(dir).unwrap();
    add_never_synced(dir, || drop(ok(alt(dir, &["tag", "v1"]))));
    let readers: [&[&str]; 6] = [
        &["log", "-n", "1"],
        &["rev-parse", "HEAD"],
        &["status"],
        &["cat-file", "-p", "HEAD"],
        &["branch"],
        &["tag"],
    ];
    for reader in readers {
        let read = alt(dir, reader);
        assert!(read.status.success(), "{reader:?}: {}", stderr(&read));
        assert!(left_out(&read, 3), "{reader:?}: {}", stderr(&read));
        assert!(!stdout(&read).contains("v1"), "{reader:?}");
    }
    let out = tmp.path().join("exported");
    let export = alt(dir, &["export", out.to_str().unwrap()]);
    assert!(export.status.success() && left_out(&export, 3));
    assert!(!out.join(".git/refs/tags/v1").exists());
    let packed = fs::read_to_string(out.join(".git/packed-refs")).unwrap_or_default();
    assert!(!packed.contains("v1"), "{packed}");

    let branch = alt(dir, &["branch", "side"]);
    let said = stderr(&branch);
    assert!(branch.status.success(), "{said}");
    assert!(
        said.contains("warning: discarded 3 operation(s) at the end of the operation log"),
        "{said}"
    );
    assert_eq!(ok(alt(dir, &["tag"])), "");
    ok(alt(dir, &["add", "new.txt"]));
    ok(alt(dir, &["fsck"]));
}

/// A daemon that finds such an intent in the store it opens hands a
/// command that only reads back to the client, which answers without it.
#[test]
fn a_daemon_answers_no_reader_from_an_add_that_was_never_synced() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    add_never_synced(dir, || drop(ok(alt(dir, &["tag", "v1"]))));
    let served = |args: &[&str]| {
        let mut command = alt_command(dir, args);
        command.env_remove("ALT_NO_DAEMON");
        command.env("ALT_DAEMON_IDLE_MS", "6000").output().unwrap()
    };
    ok(served(&["branch"]));
    let daemon = fs::read(dir.join(".alt/daemon.meta")).unwrap();
    for args in [&["tag"][..], &["rev-parse", "HEAD"], &["status"]] {
        let (direct, through) = (alt(dir, args), served(args));
        assert!(left_out(&direct, 3), "{direct:?}");
        assert_eq!(
            (through.status.code(), &through.stdout, stderr(&through)),
            (direct.status.code(), &direct.stdout, stderr(&direct)),
            "alt {args:?}"
        );
    }
    let still = fs::read(dir.join(".alt/daemon.meta")).unwrap();
    assert_eq!(still, daemon, "another daemon took over");
}

/// A writer that cut a commit which was never confirmed leaves the index
/// as the commit's add wrote it. Where the blobs went with the cut, the
/// command says which files to add again and the check reports them until
/// they are; where the blobs are stored, neither says anything.
#[test]
fn the_command_that_cuts_names_the_files_whose_blobs_went_and_no_others() {
    for lost in [Lost::Everything, Lost::Payloads, Lost::NotesCommit] {
        let tmp = alt_os::temp::tempdir().unwrap();
        let dir = tmp.path();
        power_cut(dir, lost);
        let cut = alt(dir, &["tag", "after"]);
        let said = stderr(&cut);
        assert!(cut.status.success(), "{lost:?}: {said}");
        assert!(said.contains("warning: discarded "), "{lost:?}: {said}");
        let entry = said
            .lines()
            .find_map(|line| line.strip_prefix("warning: index entry b.txt names blob "));
        if let Lost::NotesCommit = lost {
            // the blob of b.txt was stored before the commit that was lost
            assert_eq!(entry, None, "{said}");
            assert!(!said.contains("index entry"), "{said}");
            ok(alt(dir, &["fsck"]));
            ok(alt(dir, &["commit", "-m", "second, again"]));
            ok(alt(dir, &["fsck"]));
            continue;
        }
        let (blob, rest) = entry
            .unwrap_or_else(|| panic!("{lost:?}: {said}"))
            .split_once(", ")
            .unwrap();
        assert_eq!(rest, "which is not stored: add the file again");
        assert!(!said.contains("index entry a.txt"), "{said}");
        assert!(
            !alt(dir, &["cat-file", "-t", blob]).status.success(),
            "{lost:?}"
        );
        let check = alt(dir, &["fsck"]);
        assert_eq!(check.status.code(), Some(128), "{lost:?}");
        let expected =
            format!("index entry b.txt names blob {blob}, which is not stored: add the file again");
        assert!(stderr(&check).contains(&expected), "{}", stderr(&check));
        // a command that cuts nothing says nothing of the index
        let quiet = alt(dir, &["tag", "later"]);
        assert!(quiet.status.success() && !stderr(&quiet).contains("index entry"));
        ok(alt(dir, &["add", "b.txt"]));
        ok(alt(dir, &["fsck"]));
    }
}

/// Few files are looked up in the store one by one, many by comparing
/// the index with HEAD: both find the blobs that are gone.
#[test]
fn files_staged_and_lost_are_stored_again_however_many_are_named() {
    for count in [3, 40] {
        let tmp = alt_os::temp::tempdir().unwrap();
        let dir = tmp.path();
        ok(alt(dir, &["init"]));
        fs::write(dir.join("a.txt"), "one\n").unwrap();
        ok(alt(dir, &["add", "a.txt"]));
        ok(alt(dir, &["commit", "-m", "first"]));
        for n in 0..count {
            fs::write(
                dir.join(format!("new{n}.txt")),
                format!("never arrived {n}\n"),
            )
            .unwrap();
        }
        lose_objects_of(dir, || drop(ok(alt(dir, &["add", "."]))));
        assert!(!alt(dir, &["commit", "-m", "second"]).status.success());
        ok(alt(dir, &["add", "."]));
        ok(alt(dir, &["commit", "-m", "second"]));
        ok(alt(dir, &["fsck"]));
    }
}

fn git(repo: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .env("GIT_COMMITTER_NAME", "tester")
        .env("GIT_COMMITTER_EMAIL", "t@e")
        .args(args)
        .output()
        .unwrap();
    ok(out)
}

const CRLF: &str = "one\r\ntwo\r\n";

/// One commit with `* text=auto`, then `crlf.txt` staged and its blob
/// lost: the line endings of an `auto` path are decided by what the index
/// holds for it, and that can no longer be read.
fn crlf_staged_and_lost(dir: &Path) {
    ok(alt(dir, &["init"]));
    fs::write(dir.join(".gitattributes"), "* text=auto\n").unwrap();
    ok(alt(dir, &["add", ".gitattributes"]));
    ok(alt(dir, &["commit", "-m", "first"]));
    fs::write(dir.join("crlf.txt"), CRLF).unwrap();
    lose_objects_of(dir, || drop(ok(alt(dir, &["add", "crlf.txt"]))));
}

/// The same in git: the staged blob of `crlf.txt` is removed from the
/// object directory.
fn crlf_staged_and_lost_in_git(dir: &Path) {
    git(dir, &["init", "-q", "-b", "main", "."]);
    fs::write(dir.join(".gitattributes"), "* text=auto\n").unwrap();
    git(dir, &["add", ".gitattributes"]);
    git(dir, &["commit", "-qm", "first"]);
    fs::write(dir.join("crlf.txt"), CRLF).unwrap();
    git(dir, &["add", "crlf.txt"]);
    let staged = git(dir, &["ls-files", "-s", "crlf.txt"]);
    let id = staged.split_whitespace().nth(1).unwrap();
    fs::remove_file(dir.join(".git/objects").join(&id[..2]).join(&id[2..])).unwrap();
}

fn tree_of(head: &str) -> &str {
    head.lines().next().unwrap()
}

/// Git takes an index entry whose blob it cannot read for one without
/// CRLF, and so stores the file as it stores a new one.
#[test]
fn a_text_file_whose_staged_blob_is_lost_is_added_again_as_a_new_file_is() {
    for edit in ["", "three\r\n"] {
        let (ours, theirs) = (
            alt_os::temp::tempdir().unwrap(),
            alt_os::temp::tempdir().unwrap(),
        );
        let (dir, reference) = (ours.path(), theirs.path());
        crlf_staged_and_lost(dir);
        crlf_staged_and_lost_in_git(reference);
        for root in [dir, reference] {
            fs::write(root.join("crlf.txt"), format!("{CRLF}{edit}")).unwrap();
        }
        ok(alt(dir, &["add", "crlf.txt"]));
        ok(alt(dir, &["commit", "-m", "second"]));
        ok(alt(dir, &["fsck"]));
        // git only stores again a file whose stat changed
        git(reference, &["add", "--renormalize", "crlf.txt"]);
        git(reference, &["commit", "-qm", "second"]);
        assert_eq!(
            tree_of(&ok(alt(dir, &["cat-file", "-p", "HEAD"]))),
            tree_of(&git(reference, &["cat-file", "-p", "HEAD"])),
            "after {edit:?}"
        );
    }
}

#[test]
fn status_reads_a_changed_text_file_whose_staged_blob_is_lost() {
    let (ours, theirs) = (
        alt_os::temp::tempdir().unwrap(),
        alt_os::temp::tempdir().unwrap(),
    );
    let (dir, reference) = (ours.path(), theirs.path());
    crlf_staged_and_lost(dir);
    crlf_staged_and_lost_in_git(reference);
    for root in [dir, reference] {
        fs::write(root.join("crlf.txt"), format!("{CRLF}three\r\n")).unwrap();
    }
    // staged as new, and changed since
    let lines = ["new file:   crlf.txt", "modified:   crlf.txt"];
    let theirs = git(reference, &["status"]);
    let status = alt(dir, &["status"]);
    assert!(status.status.success(), "{}", stderr(&status));
    for line in lines {
        assert!(theirs.contains(line), "{theirs}");
        assert!(stdout(&status).contains(line), "{}", stdout(&status));
    }
}
