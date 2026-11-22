//! A repository whose logs end in an unfinished record, under each kind of
//! synced marker a repository can come with, stays usable: commands that
//! read succeed, and the next commit cuts the record off and leaves
//! markers that record how far the files are whole.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::process::Output;

fn alt(repo: &Path, relaxed: bool, args: &[&str]) -> Output {
    let mut command = crate::alt_command();
    command
        .current_dir(repo)
        .env("ALT_NO_DAEMON", "1")
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .env_remove("ALT_RELAXED_DURABILITY")
        .args(args);
    if relaxed {
        command.env("ALT_RELAXED_DURABILITY", "1");
    }
    command.output().unwrap()
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

fn commit(repo: &Path, relaxed: bool, tag: &str, files: usize) {
    for n in 0..files {
        fs::write(repo.join(format!("{tag}-{n}.txt")), format!("{tag} {n}\n")).unwrap();
    }
    ok(alt(repo, relaxed, &["add", "."]));
    ok(alt(repo, relaxed, &["commit", "-m", tag]));
}

/// A repository with three commits.
fn repo(dir: &Path) {
    ok(alt(dir, false, &["init"]));
    for tag in ["one", "two", "three"] {
        commit(dir, false, tag, 3);
    }
}

fn len(path: &Path) -> u64 {
    fs::metadata(path).unwrap().len()
}

fn append(path: &Path, bytes: &[u8]) {
    let mut file = OpenOptions::new().append(true).open(path).unwrap();
    file.write_all(bytes).unwrap();
}

/// Replaces both markers with what an older alt writes: the lengths of the
/// files at its last fsync and nothing else.
fn older_markers(alt_dir: &Path) {
    let lens = [
        len(&alt_dir.join("store/pack-00000001.altpack")),
        len(&alt_dir.join("store/manifests/blobmap")),
        len(&alt_dir.join("map.alt")),
    ];
    fs::write(
        alt_dir.join("odb.durable"),
        lens.map(u64::to_le_bytes).concat(),
    )
    .unwrap();
    let log = len(&alt_dir.join("oplog/log"));
    fs::write(alt_dir.join("oplog/durable"), log.to_le_bytes()).unwrap();
}

/// Makes the repository one that only an older alt wrote: no markers, no
/// head record, nothing in the oplog's lock file.
fn no_markers(alt_dir: &Path) {
    fs::remove_file(alt_dir.join("odb.durable")).unwrap();
    fs::remove_file(alt_dir.join("oplog/durable")).unwrap();
    fs::write(alt_dir.join("odb.lock"), b"").unwrap();
    fs::write(alt_dir.join("oplog/sync.lock"), b"").unwrap();
}

/// The flags of the slot in use of a marker whose slots start at `first`
/// and are `slot` bytes long, the flags `flags` bytes into each.
fn marker_flags(path: &Path, first: usize, slot: usize, flags: usize) -> u32 {
    let data = fs::read(path).unwrap();
    assert_eq!(data.len(), first + 2 * slot, "{}", path.display());
    assert_eq!(&data[first - 8..first - 4], b"ALT2");
    let serial = |at: usize| u64::from_le_bytes(data[at..at + 8].try_into().unwrap());
    let at = match serial(first) > serial(first + slot) {
        true => first,
        false => first + slot,
    };
    u32::from_le_bytes(data[at + flags..at + flags + 4].try_into().unwrap())
}

pub(super) fn odb_flags(alt_dir: &Path) -> u32 {
    marker_flags(&alt_dir.join("odb.durable"), 32, 96, 16)
}

pub(super) fn oplog_flags(alt_dir: &Path) -> u32 {
    marker_flags(&alt_dir.join("oplog/durable"), 16, 36, 8)
}

/// Reads succeed, then a commit does, every commit is still there and
/// both markers record boundaries.
fn usable(dir: &Path, commits: usize) {
    for args in [&["status"][..], &["log", "-n", "1"], &["fsck"]] {
        ok(alt(dir, false, args));
    }
    commit(dir, false, "after", 1);
    let log = ok(alt(dir, false, &["log"]));
    assert_eq!(log.lines().count(), commits + 1, "{log}");
    ok(alt(dir, false, &["fsck"]));
    let alt_dir = dir.join(".alt");
    assert_eq!((odb_flags(&alt_dir), oplog_flags(&alt_dir)), (0, 0));
}

#[test]
fn an_unfinished_record_under_an_older_marker_is_passed_over_and_cut_by_the_next_commit() {
    for file in ["map.alt", "oplog/log", "store/pack-00000001.altpack"] {
        let tmp = alt_os::temp::tempdir().unwrap();
        repo(tmp.path());
        let alt_dir = tmp.path().join(".alt");
        older_markers(&alt_dir);
        let whole = len(&alt_dir.join(file));
        append(&alt_dir.join(file), b"half a record");
        ok(alt(tmp.path(), false, &["log", "-n", "1"]));
        assert_eq!(len(&alt_dir.join(file)), whole + 13, "a read cuts nothing");
        usable(tmp.path(), 3);
        let data = fs::read(alt_dir.join(file)).unwrap();
        assert_ne!(&data[whole as usize..][..13], b"half a record", "{file}");
    }
}

#[test]
fn the_first_commit_over_older_markers_replaces_them() {
    let tmp = alt_os::temp::tempdir().unwrap();
    repo(tmp.path());
    older_markers(&tmp.path().join(".alt"));
    usable(tmp.path(), 3);
}

#[test]
fn an_unfinished_record_in_a_repository_without_markers_is_cut_like_an_older_alt_did() {
    for file in ["map.alt", "oplog/log"] {
        let tmp = alt_os::temp::tempdir().unwrap();
        repo(tmp.path());
        let alt_dir = tmp.path().join(".alt");
        no_markers(&alt_dir);
        append(&alt_dir.join(file), b"half a record");
        usable(tmp.path(), 3);
    }
}

/// A record that does not read in what unsynced commits wrote, in the
/// boot they were made in: the check tells of it as damage, and no
/// command that only reads cuts anything or shows the commits whose
/// objects lie past it.
#[test]
fn a_record_that_does_not_read_in_what_unsynced_commits_wrote_is_told_by_the_check_and_nothing_is_cut()
 {
    let tmp = alt_os::temp::tempdir().unwrap();
    repo(tmp.path());
    let alt_dir = tmp.path().join(".alt");
    for tag in ["relaxed-1", "relaxed-2", "relaxed-3"] {
        commit(tmp.path(), true, tag, 10);
    }
    assert_eq!((odb_flags(&alt_dir), oplog_flags(&alt_dir)), (1, 1));
    ok(alt(tmp.path(), false, &["fsck"]));

    // one byte of a record twenty before the end of map.alt
    let map = alt_dir.join("map.alt");
    let at = len(&map) - 20 * 82 + 10;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&map)
        .unwrap();
    let mut byte = [0u8];
    file.read_exact_at(&mut byte, at).unwrap();
    file.write_all_at(&[byte[0] ^ 0xff], at).unwrap();
    let damaged = fs::read(&map).unwrap();
    for args in [&["fsck"][..], &["status"], &["log", "-n", "4"]] {
        let out = alt(tmp.path(), false, args);
        assert!(!out.status.success(), "alt {args:?}");
        let said = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(!said.contains("panicked"), "alt {args:?}: {said}");
        // past where the file was synced to, nothing but the check takes
        // a record that does not read for damage
        assert_eq!(
            said.contains("map.alt record corrupt"),
            args == ["fsck"],
            "alt {args:?}: {said}"
        );
    }
    assert!(fs::read(&map).unwrap() == damaged, "the file is as it was");

    // with the record restored, a commit that syncs bounds the store again
    file.write_all_at(&byte, at).unwrap();
    usable(tmp.path(), 6);
}
