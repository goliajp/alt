//! A stretch of a file past where it was synced to that holds other bytes
//! than were written, with what was appended after it as it was: pages
//! the kernel lost or never wrote. In the boot of the adds as after a
//! power cut, nothing past where a file was synced to was ever confirmed:
//! every command goes on, and what does not read is cut off by the first
//! that writes objects.

use std::fs;
use std::os::unix::fs::FileExt;
use std::path::Path;

use super::super::torn_index::tree;
use super::super::unconfirmed::{alt, ok, stderr};
use super::super::unconfirmed_tails::noise;
use super::base::{ADDED, Base, CUT};
use super::{VERIFIED, goes_on, sound};

/// What is found where the stretch was written.
#[derive(Clone, Copy, Debug)]
enum Unread {
    /// bytes of no meaning
    Noise,
    /// of a pack, where a record begins: a head of a kind there is none
    /// of, naming a length by which whole records follow it
    Head,
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

/// What each add appended to each file of the object store, in turn,
/// does not read, in the boot of the adds or after a power cut that took
/// nothing else.
fn stretches(in_boot: bool) {
    let base = Base::build();
    for (file, name) in CUT.iter().enumerate().take(4) {
        for step in 0..3 {
            let (from, to) = (base.ends[file][step], base.ends[file][step + 1]);
            if from == to {
                continue;
            }
            for how in [Unread::Noise, Unread::Head] {
                // a record of the pack begins where an add began to append
                if matches!(how, Unread::Head) && (file != 0 || to - from < 82) {
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
                    match in_boot {
                        true => goes_on_in_its_boot(&what, &base, &dir, adds_first),
                        false => goes_on(&what, &base, &dir, alt, adds_first),
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
