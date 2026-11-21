//! The repository the cuts are made in.

use std::fs;
use std::path::{Path, PathBuf};

use super::super::staged_durability::repo;
use super::super::sync_order::{FILES, lens, log_len, log_synced, objects_synced};
use super::super::unconfirmed::{alt, ok};
use super::super::unconfirmed_tails::{archive, noise};
use super::another_boot;

pub(super) const ADDED: [&str; 3] = ["b.txt", "l.bin", "z.zip"];
/// What a cut is made in: the four files of the object store, the
/// operation log, the index, and the recovery snapshots.
pub(super) const CUT: [&str; 7] = [
    FILES[0],
    FILES[1],
    FILES[2],
    FILES[3],
    "oplog/log",
    "index",
    "recovery/objects",
];

pub(super) fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap().map(|entry| entry.unwrap()) {
        let target = to.join(entry.file_name());
        match entry.file_type().unwrap().is_dir() {
            true => copy_tree(&entry.path(), &target),
            false => drop(fs::copy(entry.path(), target).unwrap()),
        }
    }
}

/// One synced commit, then three adds that were not synced: a small
/// file, one of many chunks and one stored as parts, so that every file
/// of the object store grew.
pub(super) struct Base {
    tmp: alt_os::temp::TempDir,
    pub head: String,
    /// for each of the first five of [`CUT`]: where it was synced to,
    /// where it ended after each add
    pub ends: Vec<Vec<u64>>,
    /// the recovery snapshots there were before the adds
    snapshots: Vec<std::ffi::OsString>,
}

fn snapshots(alt_dir: &Path) -> Vec<std::ffi::OsString> {
    let listing = fs::read_dir(alt_dir.join(CUT[6])).unwrap();
    listing.map(|entry| entry.unwrap().file_name()).collect()
}

fn all_lens(alt_dir: &Path) -> Vec<u64> {
    let mut all = lens(alt_dir).to_vec();
    all.push(log_len(alt_dir));
    all
}

impl Base {
    pub fn build() -> Self {
        let tmp = alt_os::temp::tempdir().unwrap();
        let dir = tmp.path().join("base");
        fs::create_dir(&dir).unwrap();
        repo(&dir);
        let head = ok(alt(&dir, &["rev-parse", "HEAD"])).trim().to_owned();
        let alt_dir = dir.join(".alt");
        let mut synced = objects_synced(&alt_dir).to_vec();
        synced.push(log_synced(&alt_dir));
        let mut ends: Vec<Vec<u64>> = synced.into_iter().map(|at| vec![at]).collect();
        let snapshots = snapshots(&alt_dir);
        fs::write(dir.join(ADDED[0]), "b\n").unwrap();
        fs::write(dir.join(ADDED[1]), noise(3, 1 << 20)).unwrap();
        fs::write(dir.join(ADDED[2]), archive(&noise(11, 4000))).unwrap();
        for name in ADDED {
            ok(alt(&dir, &["add", name]));
            for (file, len) in all_lens(&alt_dir).into_iter().enumerate() {
                ends[file].push(len);
            }
        }
        for (file, ends) in ends.iter().enumerate() {
            assert!(ends[0] < ends[3], "{} did not grow", CUT[file]);
        }
        Self {
            tmp,
            head,
            ends,
            snapshots,
        }
    }

    /// Where a cut can leave file `file`: where it was synced to, the
    /// end of what each add appended, inside what each add appended, and
    /// as it is. `None` is the file as it is.
    pub fn cuts(&self, file: usize) -> Vec<Option<u64>> {
        if file >= 5 {
            // the index: as it is, or empty; the snapshots the adds
            // wrote: there, or gone
            return vec![None, Some(0)];
        }
        let ends = &self.ends[file];
        let mut cuts = vec![ends[0]];
        for step in ends.windows(2).filter(|step| step[0] < step[1]) {
            cuts.extend([step[0] + 1, (step[0] + step[1]) / 2, step[1] - 1, step[1]]);
        }
        cuts.sort_unstable();
        cuts.dedup();
        let whole = cuts.pop();
        assert_eq!(whole, ends.last().copied());
        cuts.into_iter().map(Some).chain([None]).collect()
    }

    /// A copy of the repository as the power cut left it.
    pub fn left(&self, name: &str, cuts: &[(usize, Option<u64>)]) -> PathBuf {
        let dir = self.tmp.path().join(name);
        copy_tree(&self.tmp.path().join("base"), &dir);
        let alt_dir = dir.join(".alt");
        another_boot(&alt_dir);
        for (file, at) in cuts {
            let Some(at) = *at else { continue };
            let path = alt_dir.join(CUT[*file]);
            match *file {
                // an index is put in place whole, as a new file
                5 => crate::replace_file(&path, b""),
                6 => {
                    for name in snapshots(&alt_dir) {
                        if !self.snapshots.contains(&name) {
                            fs::remove_file(path.join(name)).unwrap();
                        }
                    }
                }
                _ => {
                    let file = fs::OpenOptions::new().write(true).open(path);
                    file.unwrap().set_len(at).unwrap();
                }
            }
        }
        dir
    }
}
