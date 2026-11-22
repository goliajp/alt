//! A stretch of a file past where it was synced to that holds other bytes
//! than were written, with what was appended after it as it was: pages
//! the kernel lost or never wrote. In the boot of the adds as after a
//! power cut, nothing past where a file was synced to was ever confirmed:
//! every command goes on, and what does not read is cut off by the first
//! that writes objects; in the operation log, by the first that writes.

use std::fs;
use std::os::unix::fs::FileExt;
use std::path::Path;

use super::super::torn_index::tree;
use super::super::unconfirmed::{alt, ok, stderr};
use super::super::unconfirmed_tails::noise;
use super::base::{ADDED, Base, CUT};
use super::{VERIFIED, goes_on, sound};

/// What the check says of a record that does not read among what the
/// running boot left unsynced.
const LEFT_UNREAD: &str = "lies past where its file was last synced to";

/// What is found where the stretch was written.
#[derive(Clone, Copy, Debug)]
enum Unread {
    /// bytes of no meaning
    Noise,
    /// of a pack, where a record begins: a head of a kind there is none
    /// of, naming a length by which whole records follow it
    Head,
    /// of the operation log, where a record begins: one byte of what
    /// the record holds is another, its length as it was, so that the
    /// records after it are found
    Byte,
}

fn spoil(path: &Path, (from, to): (u64, u64), how: Unread) {
    let file = fs::OpenOptions::new().write(true).open(path).unwrap();
    match how {
        Unread::Noise => {
            let bytes = noise(from, (to - from) as usize);
            file.write_all_at(&bytes, from).unwrap();
        }
        Unread::Head => {
            file.write_all_at(&[0xff], from + 32).unwrap();
            file.write_all_at(&[0; 4], from + 37).unwrap();
        }
        Unread::Byte => {
            let was = fs::read(path).unwrap()[from as usize + 14];
            file.write_all_at(&[!was], from + 14).unwrap();
        }
    }
}

/// In the boot of the adds: a command that reads goes on, and the check
/// tells of damage and changes nothing. The first command that stores an
/// object or settles the store cuts the file: an add of what is staged
/// (`adds_first`) where it finds a blob gone, or the commit, which is
/// then refused over what went and goes through once that is added
/// again.
fn goes_on_in_its_boot(what: &str, base: &Base, dir: &Path, adds_first: bool) {
    let status = alt(dir, &["status"]);
    sound(what, &status);
    assert!(status.status.success(), "{what}: {}", stderr(&status));
    let check = alt(dir, &["fsck"]);
    assert!(!check.status.success(), "{what}: fsck passed");
    assert!(!stderr(&check).contains("panicked"), "{what}");
    // told apart from damage to what was synced; where all that does
    // not read is the end of its file, the check tells of no record
    // and names the staged file whose blob went
    let said = ok_or_said(&check);
    let cut_by = "The next command that stores objects cuts";
    let left = said.contains(LEFT_UNREAD) && said.contains(cut_by);
    let staged = said.contains("which is not stored: add the file again");
    let record = said.contains("corrupt") || said.contains("damaged") || said.contains("reads up");
    assert!(left || (staged && !record), "{what}: {said}");
    let mut add = vec!["add"];
    add.extend(ADDED);
    // an add of what is staged cuts where it finds a blob no longer
    // stored, and stores the files again
    let mut cut = false;
    if adds_first {
        let added = alt(dir, &add);
        sound(what, &added);
        assert!(added.status.success(), "{what}: {}", stderr(&added));
        cut = stderr(&added).contains("discarded ");
    }
    let committed = alt(dir, &["commit", "-m", "second"]);
    sound(what, &committed);
    let said = stderr(&committed);
    if !committed.status.success() {
        assert!(said.contains("add the file again"), "{what}: {said}");
        assert_ne!(said.contains("discarded "), cut, "{what}: {said}");
        assert_eq!(ok(alt(dir, &["rev-parse", "HEAD"])).trim(), base.head);
        let added = alt(dir, &add);
        assert!(added.status.success(), "{what}: {}", stderr(&added));
        let committed = alt(dir, &["commit", "-m", "second"]);
        assert!(committed.status.success(), "{what}: {}", stderr(&committed));
    } else {
        assert!(cut, "{what}: nothing was cut: {said}");
    }
    assert!(ok(alt(dir, &["status"])).contains("nothing to commit"));
    let check = alt(dir, &["fsck"]);
    assert!(stderr(&check).is_empty(), "{what}: {}", stderr(&check));
    assert!(ok(check).ends_with(VERIFIED), "{what}");
    let tree = tree(dir);
    for name in ADDED {
        let entry = tree.lines().find(|line| line.ends_with(name));
        let entry = entry.unwrap_or_else(|| panic!("{what}: {tree}"));
        let oid = entry.split_whitespace().nth(2).unwrap();
        let stored = alt(dir, &["cat-file", "-p", oid]);
        assert_eq!(stored.stdout, fs::read(dir.join(name)).unwrap(), "{what}");
    }
}

/// The operation log in the boot of the adds: a command that reads
/// answers from the log as it was before the stretch and changes
/// nothing, telling of the operations past it that read; the check does
/// not pass where there are such. The next command that writes cuts the
/// log, and every command goes on as after a power cut.
fn the_log_goes_on_in_its_boot(what: &str, base: &Base, dir: &Path, adds_first: bool, last: bool) {
    let log = dir.join(".alt/oplog/log");
    let before = fs::read(&log).unwrap();
    let status = alt(dir, &["status"]);
    sound(what, &status);
    assert!(status.status.success(), "{what}: {}", stderr(&status));
    let left_out = "were never synced to disk and follow a record that does not read";
    assert_ne!(stderr(&status).contains(left_out), last, "{what}");
    let check = alt(dir, &["fsck"]);
    assert!(!ok_or_said(&check).contains("panicked"), "{what}");
    if !last {
        assert!(!check.status.success(), "{what}: fsck passed");
        let said = ok_or_said(&check);
        assert!(
            said.contains("oplog/log reads up to byte"),
            "{what}: {said}"
        );
        assert!(said.contains(LEFT_UNREAD), "{what}: {said}");
        assert!(said.contains("The next command that writes cuts"), "{what}");
    }
    assert!(
        fs::read(&log).unwrap() == before,
        "{what}: a read cut the log"
    );
    // a command that writes, and here writes nothing, cuts the log
    let wrote = alt(dir, &["branch", "-d", "no-such-branch"]);
    let said = stderr(&wrote);
    assert!(
        !said.contains("panicked") && !said.contains("corrupt"),
        "{what}: {said}"
    );
    let cut = said.contains("discarded ") && said.contains("at the end of oplog/log");
    assert!(cut, "{what}: {said}");
    goes_on(what, base, dir, alt, adds_first);
}

/// What a command printed, on either stream.
fn ok_or_said(out: &std::process::Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&out.stdout), stderr(out))
}

/// What each add appended to each file of the object store and to the
/// operation log, in turn, does not read, in the boot of the adds or
/// after a power cut that took nothing else.
fn stretches(in_boot: bool) {
    let base = Base::build();
    for (file, name) in CUT.iter().enumerate().take(5) {
        for step in 0..3 {
            let (from, to) = (base.ends[file][step], base.ends[file][step + 1]);
            if from == to {
                continue;
            }
            for how in [Unread::Noise, Unread::Head, Unread::Byte] {
                // a record of the pack begins where an add began to append
                if matches!(how, Unread::Head) && (file != 0 || to - from < 82) {
                    continue;
                }
                // with the records of a later add after it
                if matches!(how, Unread::Byte) && (file != 4 || step == 2) {
                    continue;
                }
                for adds_first in [false, true] {
                    let what = format!(
                        "{name} {how:?} from {from} to {to}, in the boot {in_boot}, \
                         add first {adds_first}"
                    );
                    let case = format!("unread-{file}-{step}-{how:?}-{adds_first}");
                    let dir = match in_boot {
                        true => base.in_its_boot(&case),
                        false => base.left(&case, &[]),
                    };
                    spoil(&dir.join(".alt").join(name), (from, to), how);
                    match (in_boot, file) {
                        (true, 4) => {
                            the_log_goes_on_in_its_boot(&what, &base, &dir, adds_first, step == 2)
                        }
                        (true, _) => goes_on_in_its_boot(&what, &base, &dir, adds_first),
                        (false, _) => goes_on(&what, &base, &dir, alt, adds_first),
                    }
                    fs::remove_dir_all(dir).unwrap();
                }
            }
        }
    }
}

#[test]
fn a_stretch_that_does_not_read_in_the_boot_of_the_adds_leaves_every_command_going() {
    stretches(true);
}

#[test]
fn a_stretch_that_does_not_read_after_a_power_cut_leaves_every_command_going() {
    stretches(false);
}
