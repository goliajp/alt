//! What `alt fsck` says of the ends of the append files and of the refs:
//! bytes an interrupted append left, a whole record in what was passed
//! over, a file shorter than it was synced, operations that were never
//! synced, and a ref that points at an object which is not stored. It
//! changes no file whatever it finds.

use std::fs;
use std::path::Path;

use super::unconfirmed::{Lost, alt, files, ok, power_cut, stderr};

const LOGS: [&str; 5] = [
    "store/pack-00000001.altpack",
    "store/manifests/blobmap",
    "tier1",
    "map.alt",
    "oplog/log",
];

/// Two commits.
fn repo(dir: &Path) {
    ok(alt(dir, &["init"]));
    for (name, text) in [("a.txt", "one\n"), ("b.txt", "two\n")] {
        fs::write(dir.join(name), text).unwrap();
        ok(alt(dir, &["add", name]));
        ok(alt(dir, &["commit", "-m", name]));
    }
}

fn append(path: &Path, bytes: &[u8]) {
    let mut all = fs::read(path).unwrap();
    all.extend_from_slice(bytes);
    fs::write(path, all).unwrap();
}

/// Runs `alt fsck`, which is to change no file, and gives what it said.
fn fsck(dir: &Path) -> (bool, String, String) {
    let before = files(&dir.join(".alt"));
    let out = alt(dir, &["fsck"]);
    assert!(files(&dir.join(".alt")) == before, "fsck changed a file");
    let said = String::from_utf8_lossy(&out.stdout).into_owned();
    (out.status.success(), said, stderr(&out))
}

#[test]
fn the_bytes_passed_over_at_the_end_of_each_file_are_counted() {
    let tmp = alt_os::temp::tempdir().unwrap();
    repo(tmp.path());
    let (passes, said, _) = fsck(tmp.path());
    assert!(passes && !said.contains("passed over"), "{said}");
    for (n, log) in LOGS.iter().enumerate() {
        append(&tmp.path().join(".alt").join(log), &vec![0xcd; 100 + n]);
    }
    let (passes, said, errors) = fsck(tmp.path());
    assert!(passes, "{errors}");
    for (n, log) in LOGS.iter().enumerate() {
        let line = format!(
            "passed over {} byte(s) at the end of {log}: an append that never finished\n",
            100 + n
        );
        assert!(said.contains(&line), "{log}: {said}");
    }
    assert!(said.ends_with("every record verified\n"));
}

/// The first record of a file of records that vary in length, the file
/// beginning with a five-byte header and `fixed` bytes preceding the
/// record's length field, `after` following what the field counts.
fn first_record(bytes: &[u8], fixed: usize, after: usize) -> &[u8] {
    let len = u32::from_le_bytes(bytes[5 + fixed..5 + fixed + 4].try_into().unwrap()) as usize;
    &bytes[5..5 + fixed + 4 + len + after]
}

/// Makes what to append to a file out of what the file holds.
type Tail = fn(&[u8]) -> Vec<u8>;

#[test]
fn a_whole_record_in_what_was_passed_over_fails_the_check() {
    // bytes that read as no record, then a record that does
    let cases: [(&str, Tail); 3] = [
        ("map.alt", |all| [&[0xcd; 82][..], &all[5..87]].concat()),
        ("oplog/log", |all| {
            [&[0xcd; 7][..], first_record(all, 0, 8)].concat()
        }),
        ("store/pack-00000001.altpack", |all| {
            [&[0xcd; 7][..], first_record(all, 37, 0)].concat()
        }),
    ];
    for (log, tail) in cases {
        let tmp = alt_os::temp::tempdir().unwrap();
        repo(tmp.path());
        let path = tmp.path().join(".alt").join(log);
        let all = fs::read(&path).unwrap();
        let tail = tail(&all);
        append(&path, &tail);
        let skipped = if log == "map.alt" { 82 } else { 7 };
        let (passes, _, errors) = fsck(tmp.path());
        assert!(!passes, "{log}");
        let expected = format!(
            "{log} reads up to byte {}, and a whole record begins past that, at byte {}",
            all.len(),
            all.len() + skipped
        );
        assert!(errors.contains(&expected), "{log}: {errors}");
        // the commands before it pass over all of it
        ok(alt(tmp.path(), &["log", "-n", "1"]));
    }
}

#[test]
fn a_file_shorter_than_its_marker_says_was_synced_fails_the_check() {
    for (log, message) in [
        ("map.alt", "map.alt ends at byte"),
        ("oplog/log", "the operation log ends at byte"),
    ] {
        let tmp = alt_os::temp::tempdir().unwrap();
        repo(tmp.path());
        let path = tmp.path().join(".alt").join(log);
        let all = fs::read(&path).unwrap();
        let last = if log == "map.alt" {
            all.len() - 82
        } else {
            // where the last record of the log begins
            let mut at = 5;
            loop {
                let len = u32::from_le_bytes(all[at..at + 4].try_into().unwrap()) as usize;
                if at + 4 + len + 8 == all.len() {
                    break at;
                }
                at += 4 + len + 8;
            }
        };
        fs::write(&path, &all[..last]).unwrap();
        let (passes, _, errors) = fsck(tmp.path());
        assert!(!passes, "{log}");
        assert!(errors.contains(message), "{log}: {errors}");
        assert!(errors.contains("were committed and synced"), "{errors}");
    }
}

#[test]
fn operations_that_were_never_synced_are_told_and_are_not_damage() {
    for lost in [Lost::Everything, Lost::Payloads, Lost::NotesCommit] {
        let tmp = alt_os::temp::tempdir().unwrap();
        power_cut(tmp.path(), lost);
        let (passes, said, errors) = fsck(tmp.path());
        assert!(passes, "{lost:?}: {errors}");
        assert!(
            said.contains(
                "1 operation(s) at the end of the operation log were never synced and name \
                 objects that are not stored: the next command that writes discards them\n"
            ),
            "{lost:?}: {said}"
        );
        assert!(
            said.contains(
                "of records at the end of map.alt were never synced and name content that is \
                 not stored: the next command that writes cuts them off\n"
            ),
            "{lost:?}: {said}"
        );
        let json = ok(alt(tmp.path(), &["fsck", "--json"]));
        assert!(json.contains("\"notes\":[\""), "{json}");
        // once a command has written there is nothing left to tell
        ok(alt(tmp.path(), &["add", "b.txt"]));
        let (passes, said, _) = fsck(tmp.path());
        assert!(passes && !said.contains("never synced"), "{said}");
    }
}

#[test]
fn a_ref_that_points_at_an_object_which_is_not_stored_is_named() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    ok(alt(dir, &["init"]));
    fs::write(dir.join("a.txt"), "one\n").unwrap();
    ok(alt(dir, &["add", "a.txt"]));
    ok(alt(dir, &["commit", "-m", "first"]));
    // the object store as it is now under a log with one commit more,
    // every operation of it synced
    let inside = |path: &Path| {
        path.starts_with(dir.join(".alt/oplog")) || path.starts_with(dir.join(".alt/refs"))
    };
    let kept: Vec<_> = files(&dir.join(".alt"))
        .into_iter()
        .filter(|(path, _)| !inside(path))
        .collect();
    fs::write(dir.join("a.txt"), "two\n").unwrap();
    ok(alt(dir, &["add", "a.txt"]));
    ok(alt(dir, &["commit", "-m", "second"]));
    let second = ok(alt(dir, &["rev-parse", "HEAD"])).trim().to_owned();
    for (path, _) in files(&dir.join(".alt")) {
        if !inside(&path) {
            fs::remove_file(path).unwrap();
        }
    }
    for (path, bytes) in kept {
        fs::write(path, bytes).unwrap();
    }

    let (passes, said, errors) = fsck(dir);
    assert!(!passes, "{said}");
    let branch = format!("ref refs/heads/main points at {second}, which is not stored");
    assert!(errors.contains(&branch), "{errors}");
    assert!(
        errors.contains("ref refs/notes/alt/meta points at "),
        "{errors}"
    );
    assert!(!errors.contains("never synced"), "{errors}");
}
