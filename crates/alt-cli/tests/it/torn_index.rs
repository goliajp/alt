//! An index file that is not a whole index: `alt add` renames the index
//! into place and syncs nothing, so a power cut can leave the file empty,
//! cut short or filled with zeros. A command that only reads answers from
//! the last index the operation log vouches for and leaves the file; the
//! first command that writes in the workspace puts that index in place and
//! says what is no longer staged.

use std::fs;
use std::path::Path;
use std::process::Output;

use super::staged_durability::{add, power_cut, repo};
use super::unconfirmed::{alt, alt_command, ok, stderr};

mod mixed_versions;
mod states;

const INDEX: &str = ".alt/index";
const NOT_WHOLE: &str = "is not a whole index";
const VERIFIED: &str = "every record verified\n";

/// What a power cut leaves of an index file that was never synced.
#[derive(Clone, Copy)]
pub(super) enum Torn {
    Empty,
    InHeader,
    InEntry,
    NoChecksum,
    Zeros,
}

/// Puts what `how` leaves of the index file in its place, as a new file:
/// the one an add renames in is new as well.
pub(super) fn tear(index: &Path, how: Torn) -> Vec<u8> {
    let whole = fs::read(index).unwrap();
    let left = match how {
        Torn::Empty => Vec::new(),
        Torn::InHeader => whole[..8].to_vec(),
        Torn::InEntry => whole[..whole.len() / 2].to_vec(),
        Torn::NoChecksum => whole[..whole.len() - 20].to_vec(),
        Torn::Zeros => vec![0; whole.len()],
    };
    let new = index.with_extension("torn");
    fs::write(&new, &left).unwrap();
    fs::rename(new, index).unwrap();
    left
}

type Run = fn(&Path, &[&str]) -> Output;

/// Through a daemon, which the first command starts: one that ran before
/// the power cut did not outlive it.
fn served(dir: &Path, args: &[&str]) -> Output {
    let mut command = alt_command(dir, args);
    command
        .env_remove("ALT_NO_DAEMON")
        .env("ALT_DAEMON_IDLE_MS", "3000");
    command.output().unwrap()
}

pub(super) fn parents(dir: &Path) -> Vec<String> {
    let commit = ok(alt(dir, &["cat-file", "-p", "HEAD"]));
    let parents = commit.lines().filter_map(|l| l.strip_prefix("parent "));
    parents.map(str::to_owned).collect()
}

/// The entries of the tree of HEAD.
pub(super) fn tree(dir: &Path) -> String {
    let commit = ok(alt(dir, &["cat-file", "-p", "HEAD"]));
    let tree = commit.lines().next().unwrap().strip_prefix("tree ");
    ok(alt(dir, &["cat-file", "-p", tree.unwrap()]))
}

/// One synced commit, then `b.txt` added and not synced, then a power cut
/// that takes all the add wrote and leaves the index file as `how`: every
/// command goes on, the commit is there, and the add is said to be lost.
fn lost_add(how: Torn, run: Run) {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = &tmp.path().join("repo");
    fs::create_dir(dir).unwrap();
    repo(dir);
    let head = ok(alt(dir, &["rev-parse", "HEAD"]));
    add(dir, "b.txt");
    power_cut(dir);
    let torn = tear(&dir.join(INDEX), how);

    let status = run(dir, &["status"]);
    let told = stderr(&status);
    assert!(told.contains(NOT_WHOLE), "{told}");
    assert!(
        told.contains("this shows the workspace with the index operation"),
        "{told}"
    );
    let shown = ok(status);
    assert!(shown.contains("Untracked files:"), "{shown}");
    assert!(
        shown.contains("b.txt") && !shown.contains("a.txt"),
        "{shown}"
    );
    let check = ok(run(dir, &["fsck"]));
    assert!(
        check.contains("the index file is not a whole index"),
        "{check}"
    );
    assert!(check.ends_with(VERIFIED), "{check}");
    // neither changed the file
    assert_eq!(fs::read(dir.join(INDEX)).unwrap(), torn);

    let added = run(dir, &["add", "b.txt"]);
    let told = stderr(&added);
    assert!(told.contains(NOT_WHOLE), "{told}");
    assert!(
        told.contains("it was restored to the index operation"),
        "{told}"
    );
    assert!(told.contains("add the files again"), "{told}");
    ok(added);
    let committed = run(dir, &["commit", "-m", "second"]);
    assert!(!stderr(&committed).contains(NOT_WHOLE));
    ok(committed);
    assert_eq!(parents(dir), [head.trim()]);
    let status = run(dir, &["status"]);
    assert!(!stderr(&status).contains(NOT_WHOLE));
    assert!(ok(status).contains("nothing to commit"));
    let check = ok(run(dir, &["fsck"]));
    assert!(
        !check.contains(NOT_WHOLE) && check.ends_with(VERIFIED),
        "{check}"
    );
}

#[test]
fn an_index_file_left_empty_is_restored() {
    lost_add(Torn::Empty, alt);
    lost_add(Torn::Empty, served);
}

#[test]
fn an_index_file_cut_in_its_header_is_restored() {
    lost_add(Torn::InHeader, alt);
    lost_add(Torn::InHeader, served);
}

#[test]
fn an_index_file_cut_in_an_entry_is_restored() {
    lost_add(Torn::InEntry, alt);
    lost_add(Torn::InEntry, served);
}

#[test]
fn an_index_file_without_its_checksum_is_restored() {
    lost_add(Torn::NoChecksum, alt);
    lost_add(Torn::NoChecksum, served);
}

#[test]
fn an_index_file_of_zeros_is_restored() {
    lost_add(Torn::Zeros, alt);
    lost_add(Torn::Zeros, served);
}
