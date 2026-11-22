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

/// Three commits that synced nothing on top of the three of [`repo`],
/// and then one byte changed of a record twenty before the end of
/// `map.alt`, among those of the second of them: its commit and all that
/// the third stored lie past it. Returns the first of the three, and
/// `map.alt` as it then is.
fn unsynced_commits_past_a_record_that_does_not_read(dir: &Path) -> (String, Vec<u8>) {
    repo(dir);
    let alt_dir = dir.join(".alt");
    let mut first = String::new();
    for tag in ["relaxed-1", "relaxed-2", "relaxed-3"] {
        commit(dir, true, tag, 10);
        if tag == "relaxed-1" {
            first = ok(alt(dir, false, &["rev-parse", "HEAD"]))
                .trim()
                .to_owned();
        }
    }
    assert_eq!((odb_flags(&alt_dir), oplog_flags(&alt_dir)), (1, 1));
    ok(alt(dir, false, &["fsck"]));
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
    (first, fs::read(&map).unwrap())
}

/// In the boot they were made in, commits that synced nothing and whose
/// objects lie past a record that does not read were never confirmed: a
/// command that reads shows the repository without them, says so and
/// cuts nothing. The check tells of the record and does not pass.
#[test]
fn unsynced_commits_whose_objects_do_not_read_are_left_out_by_a_command_that_reads() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let (first, damaged) = unsynced_commits_past_a_record_that_does_not_read(tmp.path());
    let alt_dir = tmp.path().join(".alt");
    let log = fs::read(alt_dir.join("oplog/log")).unwrap();
    let left_out = "operation(s) in the operation log were never synced to disk and name objects \
                    that are not stored; this shows the repository as it was before them";
    for args in [&["status"][..], &["log", "-n", "9"], &["rev-parse", "HEAD"]] {
        let out = alt(tmp.path(), false, args);
        let said = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(out.status.success(), "alt {args:?}: {said}");
        assert!(said.contains(left_out), "alt {args:?}: {said}");
        let shown = String::from_utf8_lossy(&out.stdout).into_owned();
        match args[0] {
            "log" => {
                assert!(shown.contains("relaxed-1"), "{shown}");
                assert!(!shown.contains("relaxed-2"), "{shown}");
                assert_eq!(shown.lines().count(), 4, "{shown}");
            }
            "rev-parse" => assert_eq!(shown.trim(), first),
            // the index is still that of the last commit
            _ => assert!(shown.contains("relaxed-3-0.txt"), "{shown}"),
        }
    }
    let check = alt(tmp.path(), false, &["fsck"]);
    assert!(!check.status.success());
    let said = String::from_utf8_lossy(&check.stderr).into_owned();
    assert!(said.contains("map.alt record corrupt"), "{said}");
    let past = "lies past where its file was last synced to (map.alt from byte";
    assert!(said.contains(past), "{said}");
    assert!(
        said.contains("nothing that was synced is damaged"),
        "{said}"
    );
    assert!(fs::read(alt_dir.join("map.alt")).unwrap() == damaged);
    assert!(fs::read(alt_dir.join("oplog/log")).unwrap() == log);
}

/// The next command that writes, one that syncs or one that does not,
/// discards those commits with the record and says which refs they
/// moved; what was staged is added again and committed on the commit
/// before them.
#[test]
fn unsynced_commits_whose_objects_do_not_read_are_discarded_by_the_next_command_that_writes() {
    for writes in [&["add", "new.txt"][..], &["tag", "after"]] {
        let tmp = alt_os::temp::tempdir().unwrap();
        let dir = tmp.path();
        let (first, damaged) = unsynced_commits_past_a_record_that_does_not_read(dir);
        let alt_dir = dir.join(".alt");
        fs::write(dir.join("new.txt"), "new\n").unwrap();
        let wrote = alt(dir, false, writes);
        let said = String::from_utf8_lossy(&wrote.stderr).into_owned();
        assert!(wrote.status.success(), "alt {writes:?}: {said}");
        let discarded = "operation(s) at the end of the operation log: they were never synced to \
                         disk and name objects that are not stored";
        assert!(said.contains(discarded), "alt {writes:?}: {said}");
        let moved = format!("refs/heads/main: 2 update(s), from {first} to ");
        assert!(said.contains(&moved), "alt {writes:?}: {said}");
        assert!(
            said.contains("at the end of map.alt"),
            "alt {writes:?}: {said}"
        );
        // cut where the record began, twenty records before the end
        let map = fs::read(alt_dir.join("map.alt")).unwrap();
        let cut = damaged.len() - 20 * 82;
        assert!(map.len() < damaged.len() && map[..cut] == damaged[..cut]);
        assert!(map.len() <= cut + 82, "alt {writes:?}: {}", map.len());
        assert_eq!(ok(alt(dir, false, &["rev-parse", "HEAD"])).trim(), first);

        // what the commits that went had staged is stored again
        ok(alt(dir, false, &["add", "."]));
        ok(alt(dir, false, &["commit", "-m", "after"]));
        let log = ok(alt(dir, false, &["log"]));
        assert_eq!(log.lines().count(), 5, "{log}");
        assert!(
            !log.contains("relaxed-2") && log.contains("relaxed-1"),
            "{log}"
        );
        assert_eq!(ok(alt(dir, false, &["rev-parse", "HEAD~1"])).trim(), first);
        ok(alt(dir, false, &["fsck"]));
        assert!(ok(alt(dir, false, &["status"])).contains("nothing to commit"));
        // both markers record boundaries that hold, of other generations
        let flags = (odb_flags(&alt_dir), oplog_flags(&alt_dir));
        assert_eq!((flags.0 & 1, flags.1 & 1), (0, 0));
    }
}
