//! Two writers on a repository whose operation log has ops past what its
//! marker records, the marker being an older alt's: one settles the log,
//! with the object store locked and then the log; the other gives the log
//! its boundary, which has it sync the object store. Neither waits for the
//! other with a lock held that the other is waiting for.
//!
//! The second writer is the test: it takes the locks a settling writer
//! takes, at the moment the first one reads the marker with the log
//! locked. The marker is a named pipe until then, so that each read of it
//! waits to be answered.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use alt_cli::native::Store;
use alt_git_codec::ObjectId;
use alt_os::sys::{LOCK_EX, LOCK_NB, LOCK_UN, flock};
use alt_refs::{RefChange, RefTarget};

use super::unconfirmed::{alt, ok};

const MARKER: &str = ".alt/oplog/durable";
const LOG: &str = ".alt/oplog/log";

/// A repository as an older alt leaves it when it is killed between
/// appending a ref update and recording it: the marker is the eight bytes
/// of the log's length at the last fsync, and a whole op follows. Returns
/// the marker and the commit.
fn left_by_an_older_alt(dir: &Path) -> (Vec<u8>, String) {
    ok(alt(dir, &["init"]));
    fs::write(dir.join("a.txt"), "one\n").unwrap();
    ok(alt(dir, &["add", "a.txt"]));
    ok(alt(dir, &["commit", "-m", "first"]));
    let synced = fs::metadata(dir.join(LOG)).unwrap().len();
    ok(alt(dir, &["tag", "killed"]));
    assert!(synced < fs::metadata(dir.join(LOG)).unwrap().len());
    let marker = synced.to_le_bytes().to_vec();
    fs::write(dir.join(MARKER), &marker).unwrap();
    let head = ok(alt(dir, &["rev-parse", "HEAD"])).trim().to_owned();
    (marker, head)
}

fn make_the_marker_a_pipe(dir: &Path) {
    let pipe = dir.join(".alt/oplog/durable.pipe");
    let made = Command::new("mkfifo").arg(&pipe).status().unwrap();
    assert!(made.success());
    fs::rename(&pipe, dir.join(MARKER)).unwrap();
}

/// Answers each read of the marker with `marker`, until the reader holds
/// the log's lock and not the object store's: it is about to give the log
/// its boundary. That read is answered with the object store locked, as
/// by a writer that settles, and the marker put back as a file. Returns
/// the object store's lock.
fn lock_objects_at_the_boundary(dir: &Path, marker: &[u8]) -> File {
    let path = dir.join(MARKER);
    let log = File::open(dir.join(LOG)).unwrap();
    let objects = File::open(dir.join(".alt/odb.lock")).unwrap();
    for _ in 0..64 {
        // the write end opens once a reader is in its open of the pipe
        let (opened, reader) = mpsc::channel();
        let pipe = path.clone();
        thread::spawn(move || opened.send(OpenOptions::new().write(true).open(pipe)));
        let mut answer = reader
            .recv_timeout(Duration::from_secs(30))
            .expect("the writer reads the marker again")
            .unwrap();
        let log_free = flock(&log, LOCK_EX | LOCK_NB).is_ok();
        if log_free {
            flock(&log, LOCK_UN).unwrap();
        }
        let at_boundary = !log_free && flock(&objects, LOCK_EX | LOCK_NB).is_ok();
        if at_boundary {
            let file = path.with_file_name("durable.file");
            fs::write(&file, marker).unwrap();
            fs::rename(&file, &path).unwrap();
        }
        answer.write_all(marker).unwrap();
        drop(answer);
        if at_boundary {
            return objects;
        }
    }
    panic!("the writer read the marker 64 times and gave the log no boundary");
}

/// With the object store locked, takes the log's lock, as a writer that
/// settles the log does, and gives up both. Whether the log's lock came
/// within ten seconds.
fn settle_with(dir: &Path, objects: File) -> bool {
    let (taken, log) = mpsc::channel();
    let path = dir.join(LOG);
    thread::spawn(move || {
        let log = File::open(path).unwrap();
        flock(&log, LOCK_EX).unwrap();
        taken.send(log)
    });
    let log = log.recv_timeout(Duration::from_secs(10));
    drop(objects);
    log.is_ok()
}

const WAITED: &str = "a writer with the object store locked waited ten seconds for the log: \
                      the other one waits for the object store with the log locked";

#[test]
fn a_writer_giving_the_log_its_boundary_and_one_settling_it_do_not_wait_for_each_other() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    let (marker, _) = left_by_an_older_alt(dir);
    make_the_marker_a_pipe(dir);

    let writer = crate::alt_command()
        .current_dir(dir)
        .env("ALT_NO_DAEMON", "1")
        .env_remove("ALT_RELAXED_DURABILITY")
        .args(["tag", "second"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let objects = lock_objects_at_the_boundary(dir, &marker);
    let settled = settle_with(dir, objects);
    let done = writer.wait_with_output().unwrap();
    assert!(settled, "{WAITED}");
    ok(done);

    let tags = ok(alt(dir, &["tag"]));
    assert_eq!(tags.lines().collect::<Vec<_>>(), ["killed", "second"]);
    assert_eq!(fs::read(dir.join(MARKER)).unwrap().len(), 88);
    ok(alt(dir, &["fsck"]));
}

/// The same with the first writer holding the store the way the daemon
/// does, its fsyncs left to a sink.
#[test]
fn a_daemon_giving_the_log_its_boundary_and_a_writer_settling_it_do_not_wait_for_each_other() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    let (marker, head) = left_by_an_older_alt(dir);
    let mut store = Store::open(dir.join(".alt")).unwrap();
    store.set_defer_durability(true);
    make_the_marker_a_pipe(dir);

    let change = RefChange {
        name: "refs/tags/second".to_owned(),
        old: None,
        new: Some(RefTarget::Oid(ObjectId::from_hex(head.as_bytes()).unwrap())),
    };
    let daemon = thread::spawn(move || {
        let wrote = store.refs_mut().commit("daemon", 0, &[change]);
        let wrote = wrote.map_err(|e| e.to_string());
        wrote.and_then(|_| store.sink().unwrap().fsync_all().map_err(|e| e.to_string()))
    });
    let objects = lock_objects_at_the_boundary(dir, &marker);
    let settled = settle_with(dir, objects);
    let wrote = daemon.join().unwrap();
    assert!(settled, "{WAITED}");
    assert_eq!(wrote, Ok(()));

    let tags = ok(alt(dir, &["tag"]));
    assert_eq!(tags.lines().collect::<Vec<_>>(), ["killed", "second"]);
    ok(alt(dir, &["fsck"]));
}
