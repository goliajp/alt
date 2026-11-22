//! An add whose intent is in the operation log while a blob of the index
//! it prepared is not stored, where no cut of the log takes the intent
//! away: fsyncs were skipped (`ALT_RELAXED_DURABILITY`), so no boundary
//! says the intent was never synced, or the marker says it was and the
//! store lost the blob since. The add cannot be completed. The next
//! command that writes in the workspace abandons it: the index is as it
//! was before the add, a record closes the intent, and nothing asks for
//! a recovery.

use std::fs;
use std::path::Path;
use std::process::Output;

use super::index_blobs::{drop_last_op, stdout};
use super::unconfirmed::{alt, alt_command, ok, stderr};
use super::unconfirmed_tails::{LOGS, lens};

fn relaxed(dir: &Path, args: &[&str]) -> Output {
    let mut command = alt_command(dir, args);
    command.env("ALT_RELAXED_DURABILITY", "1");
    command.output().unwrap()
}

/// One commit, then `new.txt` added with fsyncs skipped and a power cut
/// before the add was over: the intent is in the log without its
/// completion, the object store is as before the add. `replaced` leaves
/// the index the add wrote, which without fsyncs can be there first.
/// Returns the index from before the add.
pub(super) fn add_cut_short(dir: &Path, replaced: bool) -> Vec<u8> {
    ok(alt(dir, &["init"]));
    fs::write(dir.join("a.txt"), "one\n").unwrap();
    ok(alt(dir, &["add", "a.txt"]));
    ok(alt(dir, &["commit", "-m", "first"]));
    let before = lens(dir);
    let index = fs::read(dir.join(".alt/index")).unwrap();
    fs::write(dir.join("new.txt"), "never arrived\n").unwrap();
    ok(alt(dir, &["add", "new.txt"]));
    for (file, len) in LOGS.iter().zip(before).take(4) {
        let file = fs::OpenOptions::new()
            .write(true)
            .open(dir.join(".alt").join(file))
            .unwrap();
        file.set_len(len).unwrap();
    }
    drop_last_op(dir);
    if !replaced {
        crate::replace_file(&dir.join(".alt/index"), &index);
    }
    index
}

fn pending(dir: &Path) -> Vec<String> {
    let listed: alt_json::Value =
        alt_json::from_str(&ok(alt(dir, &["operation", "pending", "--json"]))).unwrap();
    let operations = listed["operations"].as_array().unwrap().iter();
    operations
        .map(|op| op["operation_id"].as_str().unwrap().to_owned())
        .collect()
}

/// Whether the index names `path`.
fn staged(dir: &Path, path: &str) -> bool {
    let index = fs::read(dir.join(".alt/index")).unwrap();
    index.windows(path.len()).any(|at| at == path.as_bytes())
}

fn abandoned(out: &Output, id: &str) -> bool {
    let said = stderr(out);
    said.contains(&format!("warning: abandoned operation {id}: "))
        && said.contains("index entry new.txt of the index it prepared names blob ")
        && said.contains(", which is not stored: add the file again")
}

/// The file is added and committed after all, and the repository checks.
fn goes_on(dir: &Path) {
    assert!(pending(dir).is_empty());
    ok(alt(dir, &["add", "new.txt"]));
    assert!(staged(dir, "new.txt"));
    ok(alt(dir, &["commit", "-m", "second"]));
    ok(alt(dir, &["fsck"]));
}

#[test]
fn the_next_command_that_writes_abandons_an_add_that_cannot_be_completed() {
    for replaced in [false, true] {
        for skipping in [false, true] {
            let case = format!("index replaced {replaced}, fsyncs skipped {skipping}");
            let tmp = alt_os::temp::tempdir().unwrap();
            let dir = tmp.path();
            let index = add_cut_short(dir, replaced);
            let [id] = &pending(dir)[..] else {
                panic!("{case}: one operation is pending");
            };
            fs::write(dir.join("other.txt"), "other\n").unwrap();
            let add = match skipping {
                true => relaxed(dir, &["add", "other.txt"]),
                false => alt(dir, &["add", "other.txt"]),
            };
            assert!(add.status.success(), "{case}: {}", stderr(&add));
            assert!(abandoned(&add, id), "{case}: {}", stderr(&add));
            // the add that followed staged its file on the old index
            assert!(
                staged(dir, "other.txt") && !staged(dir, "new.txt"),
                "{case}"
            );
            assert_ne!(fs::read(dir.join(".alt/index")).unwrap(), index);
            let state = ok(alt(dir, &["operation", "status", id, "--json"]));
            let state: alt_json::Value = alt_json::from_str(&state).unwrap();
            assert_eq!(state["operation"]["state"], "abandoned", "{case}");
            assert_eq!(state["operation"]["outcome"], "not_applied", "{case}");
            // told once
            let again = alt(dir, &["add", "other.txt"]);
            assert!(!stderr(&again).contains("abandoned"), "{case}");
            goes_on(dir);
        }
    }
}

/// A command that writes no index abandons it as well, and so does the
/// recovery that is asked to complete it.
#[test]
fn a_recovery_and_a_command_that_moves_a_ref_abandon_it_too() {
    for command in [&["operation", "recover", "{id}"][..], &["tag", "v1"]] {
        let tmp = alt_os::temp::tempdir().unwrap();
        let dir = tmp.path();
        let index = add_cut_short(dir, true);
        let id = pending(dir).pop().unwrap();
        let args: Vec<&str> = command
            .iter()
            .map(|arg| if *arg == "{id}" { id.as_str() } else { arg })
            .collect();
        let ran = alt(dir, &args);
        assert!(ran.status.success(), "{command:?}: {}", stderr(&ran));
        assert!(abandoned(&ran, &id), "{command:?}: {}", stderr(&ran));
        if command[0] == "operation" {
            assert_eq!(stdout(&ran), format!("{id} abandoned\n"));
            let undo = alt(dir, &["undo"]);
            let said = stderr(&undo);
            assert!(!undo.status.success(), "{said}");
            assert!(said.contains("the last operation was abandoned"), "{said}");
        }
        assert_eq!(fs::read(dir.join(".alt/index")).unwrap(), index);
        goes_on(dir);
    }
}

/// An index that is neither the one from before the add nor the one it
/// prepared was put in place after it by a command whose record the log
/// does not hold: it is kept, and the add is abandoned.
#[test]
fn an_index_in_a_third_state_is_kept_and_the_add_abandoned() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    add_cut_short(dir, true);
    let id = pending(dir).pop().unwrap();
    // what a later add staged, as far as the index tells
    fs::write(dir.join("later.txt"), "later\n").unwrap();
    let other = alt_os::temp::tempdir().unwrap();
    ok(alt(other.path(), &["init"]));
    fs::write(other.path().join("a.txt"), "one\n").unwrap();
    fs::write(other.path().join("later.txt"), "later\n").unwrap();
    ok(alt(other.path(), &["add", "a.txt", "later.txt"]));
    let third = fs::read(other.path().join(".alt/index")).unwrap();
    crate::replace_file(&dir.join(".alt/index"), &third);
    let add = alt(dir, &["add", "new.txt", "later.txt"]);
    let said = stderr(&add);
    assert!(
        said.contains(&format!("warning: abandoned operation {id}: ")),
        "{said}"
    );
    assert!(said.contains("What the index stages now is kept"), "{said}");
    ok(add);
    assert!(pending(dir).is_empty());
    assert!(staged(dir, "later.txt") && staged(dir, "new.txt"));
    goes_on(dir);
}

/// The index from before the add names a blob that an earlier add left
/// unsynced and the power cut took as well: the add is abandoned all the
/// same, and the earlier file is named like any staged one whose blob is
/// lost.
#[test]
fn an_add_is_abandoned_though_the_index_from_before_it_names_a_lost_blob_as_well() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    ok(alt(dir, &["init"]));
    fs::write(dir.join("a.txt"), "one\n").unwrap();
    ok(alt(dir, &["add", "a.txt"]));
    ok(alt(dir, &["commit", "-m", "first"]));
    let before = lens(dir);
    fs::write(dir.join("earlier.txt"), "earlier\n").unwrap();
    ok(alt(dir, &["add", "earlier.txt"]));
    fs::write(dir.join("new.txt"), "never arrived\n").unwrap();
    ok(alt(dir, &["add", "new.txt"]));
    for (file, len) in LOGS.iter().zip(before).take(4) {
        let file = fs::OpenOptions::new()
            .write(true)
            .open(dir.join(".alt").join(file));
        file.unwrap().set_len(len).unwrap();
    }
    drop_last_op(dir);
    let id = pending(dir).pop().unwrap();
    let add = alt(dir, &["add", "new.txt", "earlier.txt"]);
    let said = stderr(&add);
    assert!(
        said.contains(&format!("warning: abandoned operation {id}: ")),
        "{said}"
    );
    assert!(!said.contains("recovery required"), "{said}");
    ok(add);
    assert!(pending(dir).is_empty());
    assert!(staged(dir, "earlier.txt") && staged(dir, "new.txt"));
    ok(alt(dir, &["commit", "-m", "second"]));
    ok(alt(dir, &["fsck"]));
}
