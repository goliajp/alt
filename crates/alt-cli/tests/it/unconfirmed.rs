//! A power cut during a commit: the operation log kept the ref transaction
//! and the object store lost the objects it names, because the kernel
//! wrote the log out first. Commands that read answer from the log as it
//! stood before the transaction and change nothing; the next command that
//! writes cuts it off, and the commit is made again.

use std::fs;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const PACK: &str = ".alt/store/pack-00000001.altpack";
const MARKERS: [&str; 2] = [".alt/odb.durable", ".alt/oplog/durable"];

pub(super) fn alt_command(repo: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_alt"));
    command
        .current_dir(repo)
        .env("ALT_NO_DAEMON", "1")
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .env_remove("ALT_RELAXED_DURABILITY")
        .args(args);
    command
}

pub(super) fn alt(repo: &Path, args: &[&str]) -> Output {
    alt_command(repo, args).output().unwrap()
}

pub(super) fn ok(o: Output) -> String {
    assert!(
        o.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout).unwrap()
}

pub(super) fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// Text that does not compress away.
fn precious() -> String {
    let mut word = 0x9e37_79b9_7f4a_7c15u64;
    (0..400)
        .map(|_| {
            word ^= word << 13;
            word ^= word >> 7;
            word ^= word << 17;
            format!("{word:016x}\n")
        })
        .collect()
}

/// Where the records of `pack` from byte `from` on begin.
fn records(pack: &[u8], from: usize) -> Vec<usize> {
    let (mut at, mut starts) = (from, Vec::new());
    while at + 41 <= pack.len() {
        starts.push(at);
        at += 41 + u32::from_le_bytes(pack[at + 37..at + 41].try_into().unwrap()) as usize;
    }
    starts
}

/// Every file under `dir` with its bytes.
pub(super) fn files(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    let mut entries: Vec<_> = fs::read_dir(dir).unwrap().map(|e| e.unwrap()).collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        if entry.file_type().unwrap().is_dir() {
            out.extend(files(&entry.path()));
        } else {
            out.push((entry.path(), fs::read(entry.path()).unwrap()));
        }
    }
    out
}

#[derive(Clone, Copy, Debug)]
pub(super) enum Lost {
    /// the pack ends where it did before the add
    Everything,
    /// the records are there, their payloads are zeros
    Payloads,
    /// only the last object written, the commit of the notes
    NotesCommit,
}

/// A repository with one commit and a branch `without` at it, then
/// `b.txt` added and committed, and then the state a power cut can leave:
/// the markers as they were before the add, the operation log and
/// `map.alt` with everything, the pack having lost `lost`. Returns the id
/// of the first commit and of the one that was lost.
pub(super) fn power_cut(dir: &Path, lost: Lost) -> (String, String) {
    ok(alt(dir, &["init"]));
    fs::write(dir.join("a.txt"), "one\n").unwrap();
    ok(alt(dir, &["add", "a.txt"]));
    ok(alt(dir, &["commit", "-m", "first"]));
    ok(alt(dir, &["branch", "without"]));
    let first = ok(alt(dir, &["rev-parse", "HEAD"])).trim().to_owned();
    let pack = dir.join(PACK);
    let synced = fs::metadata(&pack).unwrap().len() as usize;
    let markers = MARKERS.map(|marker| fs::read(dir.join(marker)).unwrap());

    fs::write(dir.join("b.txt"), precious()).unwrap();
    ok(alt(dir, &["add", "b.txt"]));
    ok(alt(dir, &["commit", "-m", "second"]));
    let second = ok(alt(dir, &["rev-parse", "HEAD"])).trim().to_owned();

    let mut bytes = fs::read(&pack).unwrap();
    let starts = records(&bytes, synced);
    assert!(starts.len() >= 4, "blob, tree, commit and notes");
    match lost {
        Lost::Everything => bytes.truncate(synced),
        Lost::Payloads => bytes[synced + 41..].fill(0),
        Lost::NotesCommit => bytes.truncate(*starts.last().unwrap()),
    }
    fs::write(&pack, bytes).unwrap();
    for (marker, saved) in MARKERS.iter().zip(markers) {
        fs::write(dir.join(marker), saved).unwrap();
    }
    // the lock file's note of how far the pack was synced
    let lock = fs::OpenOptions::new()
        .write(true)
        .open(dir.join(".alt/odb.lock"))
        .unwrap();
    lock.write_all_at(&[0u8; 24], 1176).unwrap();
    (first, second)
}

#[test]
fn a_commit_the_log_kept_and_the_store_lost_is_passed_over_cut_and_made_again() {
    for lost in [Lost::Everything, Lost::Payloads, Lost::NotesCommit] {
        let tmp = alt_os::temp::tempdir().unwrap();
        let dir = tmp.path();
        let (first, second) = power_cut(dir, lost);
        let before = files(&dir.join(".alt"));

        // commands that read: the repository as it was before the commit
        let log = alt(dir, &["log", "-n", "3"]);
        assert!(stderr(&log).contains("never synced"), "{lost:?}: {log:?}");
        let log = ok(log);
        assert!(log.contains(&first) && !log.contains(&second), "{log}");
        // a command falls back when an object it reads is missing: with
        // only the notes lost, the commit itself reads, and is shown
        let whole = !matches!(lost, Lost::NotesCommit);
        let head = alt(dir, &["cat-file", "-p", "HEAD"]);
        assert_eq!(stderr(&head).contains("never synced"), whole, "{head:?}");
        let subject = if whole { "first\n" } else { "second\n" };
        assert!(ok(head).ends_with(subject), "{lost:?}");
        let status = alt(dir, &["status"]);
        assert_eq!(stderr(&status).contains("never synced"), whole, "{lost:?}");
        assert_eq!(ok(status).contains("b.txt"), whole, "{lost:?}");
        // a ref alone is read from the log as it is: no object is missed
        assert_eq!(ok(alt(dir, &["rev-parse", "HEAD"])).trim(), second);
        assert!(
            files(&dir.join(".alt")) == before,
            "{lost:?}: a command that reads changed a file"
        );

        // the first command that writes cuts the transaction off
        let add = alt(dir, &["add", "b.txt"]);
        assert!(stderr(&add).contains("discarded 1 operation(s)"), "{add:?}");
        ok(add);
        ok(alt(dir, &["commit", "-m", "second again"]));
        let log = alt(dir, &["log", "-n", "4"]);
        assert_eq!(stderr(&log), "", "{lost:?}");
        let log = ok(log);
        let subjects: Vec<_> = log.lines().filter_map(|l| l.split_once(' ')).collect();
        assert_eq!(subjects.len(), 2, "{log}");
        assert_eq!(subjects[0].1, "second again");
        assert_eq!(subjects[1], (first.as_str(), "first"));
        assert!(ok(alt(dir, &["fsck"])).ends_with("every record verified\n"));

        // out of the store again, byte for byte
        ok(alt(dir, &["switch", "without"]));
        assert!(!dir.join("b.txt").exists());
        ok(alt(dir, &["switch", "main"]));
        assert_eq!(fs::read_to_string(dir.join("b.txt")).unwrap(), precious());
        assert_eq!(fs::read_to_string(dir.join("a.txt")).unwrap(), "one\n");
        ok(alt(dir, &["fsck"]));
    }
}

/// The same loss with the operation log's marker saying the transaction
/// was synced: nothing tells it from damage. No command passes over it or
/// cuts it, and no file changes.
#[test]
fn a_synced_transaction_whose_commit_is_gone_is_damage_and_stays() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    ok(alt(dir, &["init"]));
    fs::write(dir.join("a.txt"), "one\n").unwrap();
    ok(alt(dir, &["add", "a.txt"]));
    ok(alt(dir, &["commit", "-m", "first"]));
    // the object store as it is now, the log with one commit more
    let kept: Vec<_> = files(&dir.join(".alt"))
        .into_iter()
        .filter(|(path, _)| !path.starts_with(dir.join(".alt/oplog")))
        .filter(|(path, _)| !path.starts_with(dir.join(".alt/refs")))
        .collect();
    fs::write(dir.join("a.txt"), "two\n").unwrap();
    ok(alt(dir, &["add", "a.txt"]));
    ok(alt(dir, &["commit", "-m", "second"]));
    for (path, _) in files(&dir.join(".alt")) {
        let log = path.starts_with(dir.join(".alt/oplog"));
        if !log && !path.starts_with(dir.join(".alt/refs")) {
            fs::remove_file(path).unwrap();
        }
    }
    for (path, bytes) in kept {
        fs::write(path, bytes).unwrap();
    }

    let before = files(&dir.join(".alt"));
    for args in [
        &["log", "-n", "3"][..],
        &["cat-file", "-p", "HEAD"],
        &["commit", "-m", "third", "--allow-empty"],
    ] {
        let out = alt(dir, args);
        assert!(!out.status.success(), "{args:?}: {out:?}");
        let said = stderr(&out);
        assert!(
            !said.contains("never synced") && !said.contains("discarded"),
            "{args:?}: {said}"
        );
    }
    let after = files(&dir.join(".alt"));
    let changed: Vec<_> = after
        .iter()
        .filter(|file| !before.contains(file))
        .map(|(path, _)| path.strip_prefix(dir).unwrap())
        // a command that writes journals the request it was given, first
        .filter(|path| !path.starts_with(".alt/submissions") && !path.starts_with(".alt/requests"))
        .collect();
    assert!(changed.is_empty(), "changed: {changed:?}");
}

/// A log whose marker records no boundary that holds has no operations to
/// look at: a command that writes does not wait for the locks it would
/// look at them under.
#[test]
fn a_writer_takes_no_lock_to_settle_a_log_without_a_boundary() {
    use std::sync::mpsc;
    use std::time::Duration;

    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    ok(alt(dir, &["init"]));
    fs::write(dir.join("a.txt"), "one\n").unwrap();
    ok(alt(dir, &["add", "a.txt"]));
    ok(alt(dir, &["commit", "-m", "first"]));
    // no slot of it holds
    let marker = dir.join(MARKERS[1]);
    fs::write(&marker, &fs::read(&marker).unwrap()[..16]).unwrap();

    let held = fs::File::open(dir.join(".alt/odb.lock")).unwrap();
    alt_os::sys::flock(&held, alt_os::sys::LOCK_EX).unwrap();
    let alt_dir = dir.join(".alt");
    let (done, settled) = mpsc::channel();
    let writer = std::thread::spawn(move || {
        let mut store = alt_cli::native::Store::open(alt_dir).unwrap();
        let settled = store.settle().map_err(|e| e.to_string());
        done.send(settled).unwrap();
    });
    let settled = settled.recv_timeout(Duration::from_secs(5));
    alt_os::sys::flock(&held, alt_os::sys::LOCK_UN).unwrap();
    writer.join().unwrap();
    assert_eq!(settled, Ok(Ok(())), "it waited for the object store's lock");
}
