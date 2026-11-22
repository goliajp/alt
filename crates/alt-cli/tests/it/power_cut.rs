//! A power cut after adds that were not synced: each of the files they
//! appended to, and the index, ends where the kernel happened to have
//! written it, anywhere from where it was last synced to its end and
//! each on its own. Whatever is left, every command goes on, none takes
//! an object for stored that does not read, and what was committed is
//! there.

use std::fs;
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::process::Output;

use super::torn_index::{served, tree};
use super::unconfirmed::{alt, ok, stderr};

mod base;
mod many;
mod mixed_versions;
mod pack_head;
mod refs_first;
mod unread;

use base::{ADDED, Base, CUT};

/// Where `odb.lock` notes the boot unsynced appends were made in.
const BOOT_NOTED_AT: u64 = 1224;
const VERIFIED: &str = "every record verified\n";

/// Makes the notes of `odb.lock`, of `oplog/sync.lock` and of
/// `index.seen` those of another boot: what was left unsynced is then no
/// longer taken to be as it was written.
pub(super) fn another_boot(alt_dir: &Path) {
    // the file, where its note is, how long the note and its checksum are
    let notes = [
        ("odb.lock", BOOT_NOTED_AT, 56, 8),
        ("oplog/sync.lock", 8, 16, 8),
    ];
    for (file, at, body, sum) in notes {
        let mut lock = fs::OpenOptions::new();
        let lock = lock.read(true).write(true);
        let lock = lock.open(alt_dir.join(file)).unwrap();
        let mut note = vec![0u8; body + sum];
        if lock.read_exact_at(&mut note, at).is_err() || note.iter().all(|b| *b == 0) {
            continue;
        }
        note[0] ^= 1;
        let hash = alt_hash::blake3::hash(&note[..body]);
        note[body..].copy_from_slice(&hash.as_bytes()[..sum]);
        lock.write_all_at(&note, at).unwrap();
    }
    // the note beside the index names the boot it was written in as well
    let seen = alt_dir.join("index.seen");
    if let Ok(mut note) = fs::read(&seen)
        && note.len() > 24
    {
        let body = note.len() - 8;
        note[8] ^= 1;
        let hash = alt_hash::blake3::hash(&note[..body]);
        note[body..].copy_from_slice(&hash.as_bytes()[..8]);
        fs::write(seen, note).unwrap();
    }
}

type Run = fn(&Path, &[&str]) -> Output;

/// What a command says when it found something it has no answer for.
fn sound(what: &str, out: &Output) {
    let said = format!("{}{}", String::from_utf8_lossy(&out.stdout), stderr(out));
    for word in ["not found", "panicked", "corrupt"] {
        assert!(!said.contains(word), "{what}: {said}");
    }
}

/// A check before the first command that writes passes, or says only
/// what that command will discard and which files are to be added again.
fn checked(what: &str, out: &Output) {
    sound(what, out);
    if out.status.success() {
        return;
    }
    let told = stderr(out);
    let known = |line: &str| {
        line.contains("which is not stored: add the file again")
            || line.contains("unconfirmed tail: the next command that writes")
            || line.contains("is not a whole index")
    };
    assert!(told.lines().all(known), "{what}: fsck: {told}");
}

/// Runs a command that writes. Where the cut took the completion of an
/// add and left its intent and its blobs, the command is refused and
/// names the operation: `operation recover` completes it, and the command
/// then runs.
fn writes(what: &str, dir: &Path, run: Run, args: &[&str]) -> Output {
    let out = run(dir, args);
    sound(what, &out);
    let told = stderr(&out);
    let Some(at) = told.find(" has no durable completion; recovery required") else {
        return out;
    };
    assert!(!out.status.success(), "{what}: {told}");
    let id = told[..at].rsplit(' ').next().unwrap();
    let recovered = run(dir, &["operation", "recover", id]);
    sound(what, &recovered);
    assert!(
        recovered.status.success(),
        "{what}: recover: {}",
        stderr(&recovered)
    );
    let again = run(dir, args);
    sound(what, &again);
    again
}

/// Every command goes on in `dir`, with `run` first and then directly;
/// what was committed before the cut is there and what is committed
/// after it reads.
fn goes_on(what: &str, base: &Base, dir: &Path, run: Run, adds_first: bool) {
    let status = run(dir, &["status"]);
    sound(what, &status);
    assert!(
        status.status.success(),
        "{what}: status: {}",
        stderr(&status)
    );
    checked(what, &run(dir, &["fsck"]));
    assert_eq!(ok(alt(dir, &["rev-parse", "HEAD"])).trim(), base.head);
    if !adds_first {
        // a commit of what is staged takes nothing that is not stored
        writes(what, dir, run, &["commit", "-m", "what was left"]);
        checked(what, &run(dir, &["fsck"]));
    }
    let mut add = vec!["add"];
    add.extend(ADDED);
    let added = writes(what, dir, run, &add);
    assert!(added.status.success(), "{what}: add: {}", stderr(&added));
    if !ok(alt(dir, &["status"])).contains("nothing to commit") {
        let committed = writes(what, dir, run, &["commit", "-m", "second"]);
        assert!(committed.status.success(), "{what}: {}", stderr(&committed));
    }
    assert!(ok(alt(dir, &["status"])).contains("nothing to commit"));
    let check = alt(dir, &["fsck"]);
    assert!(stderr(&check).is_empty(), "{what}: {}", stderr(&check));
    assert!(ok(check).ends_with(VERIFIED), "{what}");
    let history = ok(alt(dir, &["log"]));
    let first = history.lines().last().unwrap_or_default();
    assert!(first.starts_with(&base.head), "{what}: {history}");
    let tree = tree(dir);
    assert!(tree.contains("a.txt"), "{what}: {tree}");
    for name in ADDED {
        let entry = tree.lines().find(|line| line.ends_with(name));
        let entry = entry.unwrap_or_else(|| panic!("{what}: {tree}"));
        let oid = entry.split_whitespace().nth(2).unwrap();
        let stored = alt(dir, &["cat-file", "-p", oid]);
        assert_eq!(stored.stdout, fs::read(dir.join(name)).unwrap(), "{what}");
    }
}

/// Every pair of places the files `first` and another of [`CUT`] can be
/// left at, the others as they are.
fn pairs_with(first: usize) {
    let base = Base::build();
    let mut case = 0;
    for (second, name) in CUT.iter().enumerate().skip(first + 1) {
        for at in base.cuts(first) {
            for other in base.cuts(second) {
                case += 1;
                let what = format!(
                    "{} at {at:?} of {:?}, {} at {other:?} of {:?}",
                    CUT[first],
                    base.ends.get(first),
                    name,
                    base.ends.get(second),
                );
                let dir = base.left(&format!("{case}"), &[(first, at), (second, other)]);
                goes_on(&what, &base, &dir, alt, case % 2 == 0);
                fs::remove_dir_all(dir).unwrap();
            }
        }
    }
}

#[test]
fn a_pack_cut_anywhere_past_its_boundary_with_any_other_file_cut_leaves_every_command_going() {
    pairs_with(0);
}

#[test]
fn a_blob_map_cut_anywhere_past_its_boundary_with_any_later_file_cut_leaves_every_command_going() {
    pairs_with(1);
}

#[test]
fn a_tier1_map_cut_anywhere_past_its_boundary_with_any_later_file_cut_leaves_every_command_going() {
    pairs_with(2);
}

#[test]
fn a_map_cut_anywhere_past_its_boundary_with_any_later_file_cut_leaves_every_command_going() {
    pairs_with(3);
}

#[test]
fn a_log_cut_anywhere_past_its_boundary_with_the_index_or_the_snapshots_cut_leaves_every_command_going()
 {
    pairs_with(4);
}

#[test]
fn the_index_cut_with_the_snapshots_of_the_adds_gone_leaves_every_command_going() {
    pairs_with(5);
}

#[test]
fn every_file_cut_at_once_leaves_every_command_going() {
    let base = Base::build();
    // each file at its boundary, at its end, and one place in between,
    // the places in between shifted against each other
    for round in 0..4 {
        for torn in [false, true] {
            let mut cuts = Vec::new();
            for file in 0..5 {
                let places = base.cuts(file);
                let at = match round {
                    0 => 0,
                    3 => places.len() - 1,
                    _ => (1 + file * round) % places.len(),
                };
                cuts.push((file, places[at]));
            }
            cuts.push((5, torn.then_some(0)));
            cuts.push((6, (round % 2 == 1).then_some(0)));
            let what = format!("{cuts:?} of {:?}", base.ends);
            let dir = base.left(&format!("all-{round}-{torn}"), &cuts);
            goes_on(&what, &base, &dir, alt, torn);
        }
    }
}

#[test]
fn a_stretch_that_never_reached_the_disk_before_one_that_did_leaves_every_command_going() {
    let base = Base::build();
    for (file, name) in CUT.iter().enumerate().take(5) {
        // what each add appended, in turn, with what was appended after it
        for step in 0..3 {
            let (from, to) = (base.ends[file][step], base.ends[file][step + 1]);
            if from == to || to == base.ends[file][3] {
                continue;
            }
            for torn in [false, true] {
                let what = format!("{name} zeros from {from} to {to}, index torn {torn}");
                let dir = base.left(
                    &format!("zeros-{file}-{step}-{torn}"),
                    &[(5, torn.then_some(0))],
                );
                let path = dir.join(".alt").join(name);
                let stretch = fs::OpenOptions::new().write(true).open(path).unwrap();
                let zeros = vec![0u8; (to - from) as usize];
                stretch.write_all_at(&zeros, from).unwrap();
                goes_on(&what, &base, &dir, alt, torn);
            }
        }
    }
}

#[test]
fn a_daemon_goes_on_after_a_cut_of_any_one_file() {
    let base = Base::build();
    for (file, name) in CUT.iter().enumerate() {
        let places = base.cuts(file);
        // where it was synced to, and inside what the last add appended;
        // the index empty, the snapshots gone
        let at = match file {
            5.. => vec![Some(0)],
            _ => vec![places[0], places[places.len() - 2]],
        };
        for at in at {
            let what = format!("served, {} at {at:?} of {:?}", name, base.ends.get(file));
            let dir = base.left(&format!("served-{file}-{at:?}"), &[(file, at)]);
            goes_on(&what, &base, &dir, served, file % 2 == 0);
        }
    }
}
