//! Adds between two commands that sync grow the operation log by more
//! than a snapshot of the refs is written anew for. The snapshot stays
//! anchored where the log was synced to, so that a record past it which
//! does not read is where the log ends for every command, whichever of
//! the adds appended it.

use std::fs;
use std::path::Path;

use super::super::super::staged_durability::repo;
use super::super::super::sync_order::{log_len, log_synced};
use super::super::super::unconfirmed::{alt, ok, stderr};
use super::super::{VERIFIED, another_boot, sound, writes};
use super::{Unread, spoil};

/// One synced commit, then adds that sync nothing until the log holds
/// more past its boundary than a snapshot is written anew for, and a
/// command that reads. Where the log ended after each add.
fn grown(dir: &Path) -> Vec<u64> {
    repo(dir);
    let alt_dir = dir.join(".alt");
    let synced = log_synced(&alt_dir);
    let mut ends = vec![log_len(&alt_dir)];
    assert_eq!(ends[0], synced);
    while ends[ends.len() - 1] - synced < 24 << 10 {
        let name = format!("n{}.txt", ends.len());
        fs::write(dir.join(&name), format!("{name}\n")).unwrap();
        ok(alt(dir, &["add", &name]));
        ends.push(log_len(&alt_dir));
        assert!(ends.len() < 400, "the log does not grow: {ends:?}");
    }
    ok(alt(dir, &["status"]));
    assert_eq!(log_synced(&alt_dir), synced);
    ends
}

/// Where in the log the op ends that the snapshot of the refs is
/// anchored at.
fn anchored_at(alt_dir: &Path) -> Option<u64> {
    let snapshot = fs::read(alt_dir.join("refs/snapshot")).ok()?;
    assert_eq!((&snapshot[..4], snapshot[4]), (&b"ALTS"[..], 3));
    Some(u64::from_le_bytes(snapshot[61..69].try_into().unwrap()))
}

fn copy(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        match entry.file_type().unwrap().is_dir() {
            true => copy(&entry.path(), &target),
            false => drop(fs::copy(entry.path(), target).unwrap()),
        }
    }
}

/// What the add after `ends[step]` appended does not read, in the boot
/// of the adds or after a power cut: commands that read answer without
/// what follows it and change nothing, the first that writes cuts the
/// log there, and every command goes on.
fn goes_on_past(grown: &Path, case: &Path, ends: &[u64], step: usize, how: Unread, in_boot: bool) {
    let (from, to) = (ends[step], ends[step + 1]);
    let what = format!("{how:?} from {from} to {to}, in the boot {in_boot}");
    copy(grown, case);
    let alt_dir = case.join(".alt");
    let head = ok(alt(case, &["rev-parse", "HEAD"]));
    if !in_boot {
        another_boot(&alt_dir);
    }
    let log = alt_dir.join("oplog/log");
    spoil(&log, (from, to), how);
    let before = fs::read(&log).unwrap();
    let left_out = "were never synced to disk and follow a record that does not read";
    for args in [&["status"][..], &["log", "-n", "1"], &["rev-parse", "HEAD"]] {
        let read = alt(case, args);
        sound(&what, &read);
        let said = stderr(&read);
        assert!(read.status.success(), "{what} {args:?}: {said}");
        assert!(said.contains(left_out), "{what} {args:?}: {said}");
    }
    assert!(
        fs::read(&log).unwrap() == before,
        "{what}: a read cut the log"
    );
    assert!(
        anchored_at(&alt_dir).is_none_or(|at| at <= ends[0]),
        "{what}"
    );

    let wrote = alt(case, &["branch", "-d", "no-such-branch"]);
    let said = stderr(&wrote);
    assert!(
        !said.contains("panicked") && !said.contains("corrupt"),
        "{what}: {said}"
    );
    let cut = said.contains("discarded ") && said.contains("at the end of oplog/log");
    assert!(cut, "{what}: {said}");
    for args in [&["add", "."][..], &["commit", "-m", "second"]] {
        let wrote = writes(&what, case, alt, args);
        let said = stderr(&wrote);
        assert!(wrote.status.success(), "{what} {args:?}: {said}");
    }
    assert!(ok(alt(case, &["status"])).contains("nothing to commit"));
    let check = alt(case, &["fsck"]);
    assert!(stderr(&check).is_empty(), "{what}: {}", stderr(&check));
    assert!(ok(check).ends_with(VERIFIED), "{what}");
    let history = ok(alt(case, &["log"]));
    let first = history.lines().last().unwrap_or_default();
    assert!(first.starts_with(head.trim()), "{what}: {history}");
    fs::remove_dir_all(case).unwrap();
}

#[test]
fn a_snapshot_of_the_refs_is_anchored_no_further_than_the_log_was_synced_to() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    let ends = grown(dir);
    let alt_dir = dir.join(".alt");
    assert!(anchored_at(&alt_dir).is_none_or(|at| at <= ends[0]));
    fs::write(dir.join("one-more.txt"), "more\n").unwrap();
    for args in [
        &["add", "one-more.txt"][..],
        &["status"],
        &["log", "-n", "1"],
    ] {
        ok(alt(dir, args));
        let anchored = anchored_at(&alt_dir);
        assert!(anchored.is_none_or(|at| at <= ends[0]), "{args:?}");
    }
    // a command that syncs moves the boundary, and the snapshot follows
    ok(alt(dir, &["commit", "-m", "second"]));
    ok(alt(dir, &["status"]));
    let synced = log_synced(&alt_dir);
    assert_eq!(synced, log_len(&alt_dir));
    assert_eq!(anchored_at(&alt_dir), Some(synced));
}

/// The first of the adds left a record that a power cut took, and the
/// records after it are whole, the one a command that read had the log
/// to its end among them.
#[test]
fn a_record_lost_among_adds_that_a_command_read_to_their_end_is_where_the_log_ends() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path().join("grown");
    fs::create_dir(&dir).unwrap();
    let ends = grown(&dir);
    let case = tmp.path().join("case");
    goes_on_past(&dir, &case, &ends, 0, Unread::Noise, false);
}

#[test]
fn a_stretch_that_does_not_read_among_adds_that_outgrew_a_snapshot_leaves_every_command_going() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path().join("grown");
    fs::create_dir(&dir).unwrap();
    let ends = grown(&dir);
    let last = ends.len() - 1;
    for step in [0, last / 2, last - 2] {
        for how in [Unread::Noise, Unread::Byte] {
            for in_boot in [true, false] {
                let case = tmp.path().join(format!("{step}-{how:?}-{in_boot}"));
                goes_on_past(&dir, &case, &ends, step, how, in_boot);
            }
        }
    }
}
