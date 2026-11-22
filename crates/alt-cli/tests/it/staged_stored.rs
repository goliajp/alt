//! An add of many paths looks through everything staged for blobs the
//! store lost to a power cut. Once it has found none, the note beside
//! the index (`index.seen`) says so for as long as the machine stays up,
//! and the adds that follow do not look again. After a power cut the note
//! is that of another boot and says nothing: what was lost is found and
//! stored again.

use std::fs;
use std::path::Path;

use super::index_blobs::stdout;
use super::staged_durability::{power_cut, repo};
use super::unconfirmed::{alt, ok, stderr};

/// More files than an add asks the store about one by one.
const MANY: usize = 40;

fn write_many(dir: &Path, text: &str) {
    fs::create_dir_all(dir.join("sub")).unwrap();
    for at in 0..MANY {
        fs::write(
            dir.join(format!("sub/f{at:02}.txt")),
            format!("{text} {at}\n"),
        )
        .unwrap();
    }
}

mod taken_out;

/// Whether the note beside the index says that what it stages is stored,
/// at some generation of the store.
pub(super) fn noted_stored(dir: &Path) -> bool {
    let note = fs::read(dir.join(".alt/index.seen")).unwrap_or_default();
    note.len() == 128 && note[112..120] != [0; 8]
}

fn refused_for(dir: &Path, path: &str) {
    let commit = alt(dir, &["commit", "-m", "second"]);
    assert!(!commit.status.success(), "{}", stdout(&commit));
    let said = stderr(&commit);
    let entry = format!("index entry {path} names blob ");
    assert!(said.contains(&entry), "{said}");
}

fn committed_whole(dir: &Path) {
    ok(alt(dir, &["commit", "-m", "second"]));
    let check = alt(dir, &["fsck"]);
    assert!(stderr(&check).is_empty(), "{}", stderr(&check));
    assert!(ok(check).ends_with("every record verified\n"));
    assert!(ok(alt(dir, &["status"])).contains("nothing to commit"));
}

#[test]
fn what_a_power_cut_took_is_stored_again_though_the_index_was_noted_as_staging_what_is_stored() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    repo(dir);
    write_many(dir, "first");
    ok(alt(dir, &["add", "."]));
    // an add that asked about a few entries has not looked at them all
    assert!(!noted_stored(dir));
    ok(alt(dir, &["add", "."]));
    assert!(noted_stored(dir));
    // the index an add makes of such an index is one as well
    fs::write(dir.join("sub/f00.txt"), "changed\n").unwrap();
    ok(alt(dir, &["add", "sub/f00.txt"]));
    assert!(noted_stored(dir));
    write_many(dir, "second");
    ok(alt(dir, &["add", "."]));
    assert!(noted_stored(dir));

    // none of the adds was synced: the blobs are gone, the index stayed
    let index = fs::read(dir.join(".alt/index")).unwrap();
    power_cut(dir);
    assert_eq!(fs::read(dir.join(".alt/index")).unwrap(), index);
    refused_for(dir, "sub/f00.txt");
    ok(alt(dir, &["add", "."]));
    assert!(!noted_stored(dir));
    committed_whole(dir);
    ok(alt(dir, &["add", "."]));
    assert!(noted_stored(dir));
}

#[test]
fn an_index_is_not_noted_while_a_file_no_add_named_is_staged_with_a_lost_blob() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    repo(dir);
    write_many(dir, "first");
    fs::write(dir.join("out.txt"), "beside them\n").unwrap();
    ok(alt(dir, &["add", "."]));
    ok(alt(dir, &["add", "."]));
    assert!(noted_stored(dir));
    power_cut(dir);

    // the files named are stored again, the one beside them is not
    ok(alt(dir, &["add", "sub"]));
    assert!(!noted_stored(dir));
    ok(alt(dir, &["add", "sub"]));
    assert!(!noted_stored(dir));
    refused_for(dir, "out.txt");
    ok(alt(dir, &["add", "."]));
    committed_whole(dir);
}

#[test]
fn an_index_another_command_put_in_place_is_looked_through_anew() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    repo(dir);
    write_many(dir, "first");
    ok(alt(dir, &["add", "."]));
    ok(alt(dir, &["add", "."]));
    fs::write(dir.join("sub/f00.txt"), "changed\n").unwrap();
    ok(alt(dir, &["add", "."]));
    assert!(noted_stored(dir));
    // the index an undo puts back was staged by some earlier add
    ok(alt(dir, &["undo"]));
    assert!(!noted_stored(dir));
    ok(alt(dir, &["add", "."]));
    ok(alt(dir, &["add", "."]));
    assert!(noted_stored(dir));
}
