//! The states a writer, a power cut or an older alt can leave the store
//! of a repository in, each made out of one repository that is built once.

use std::fs;
use std::path::{Path, PathBuf};

use super::super::unconfirmed::{alt, ok};
use super::super::unconfirmed_tails::{LOGS, MARKERS, archive, damage, lag, lens, noise};

/// One thing done to a copy of the repository.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Step {
    /// the beginning of a record at the end of this log: an append that
    /// never finished
    Torn(usize),
    /// the markers as they were after the first commit
    Lag,
    /// a byte changed in the first record of this log past what the first
    /// commit wrote
    Damage(usize),
    /// the pack as it was after the first commit: what the records and
    /// operations after it name is not there
    PackLost,
    /// the markers an older alt writes: file lengths and nothing else
    OlderMarkers,
    /// no markers, and nothing that says there ever were any
    NeverMarked,
    /// both markers empty
    EmptyMarkers,
    /// nothing in `odb.lock`
    EmptyLock,
    /// no index runs
    NoRuns,
    /// every index run cut in half
    HalfRuns,
    /// the operation log cut to nothing
    EmptyLog,
    /// no Tier 1 map
    NoTier1,
}

use Step::*;

/// States by how the logs end.
pub(super) const ENDS: &[(&str, &[Step])] = &[
    ("as written", &[]),
    ("unfinished pack record", &[Torn(0)]),
    ("unfinished blob map record", &[Torn(1)]),
    ("unfinished tier1 record", &[Torn(2)]),
    ("unfinished map.alt record", &[Torn(3)]),
    ("unfinished operation", &[Torn(4)]),
    ("markers lag", &[Lag]),
    (
        "pack record past the boundary does not read",
        &[Lag, Damage(0)],
    ),
    (
        "blob map record past the boundary does not read",
        &[Lag, Damage(1)],
    ),
    (
        "tier1 record past the boundary does not read",
        &[Lag, Damage(2)],
    ),
    (
        "map.alt record past the boundary does not read",
        &[Lag, Damage(3)],
    ),
    (
        "operation past the boundary does not read",
        &[Lag, Damage(4)],
    ),
    (
        "content of records past the boundary gone",
        &[Lag, PackLost],
    ),
];

/// States by what the markers and the lock file say.
pub(super) const MARKS: &[(&str, &[Step])] = &[
    ("older markers", &[OlderMarkers]),
    (
        "older markers, unfinished map.alt record",
        &[OlderMarkers, Torn(3)],
    ),
    (
        "older markers, unfinished operation",
        &[OlderMarkers, Torn(4)],
    ),
    ("never marked", &[NeverMarked]),
    (
        "never marked, unfinished pack record",
        &[NeverMarked, Torn(0)],
    ),
    (
        "never marked, unfinished map.alt record",
        &[NeverMarked, Torn(3)],
    ),
    (
        "never marked, unfinished operation",
        &[NeverMarked, Torn(4)],
    ),
    ("markers do not read", &[EmptyMarkers]),
    (
        "markers do not read, unfinished map.alt record",
        &[EmptyMarkers, Torn(3)],
    ),
    ("odb.lock emptied", &[EmptyLock]),
    (
        "odb.lock emptied, markers lag, unfinished map.alt record",
        &[Lag, Torn(3), EmptyLock],
    ),
    (
        "odb.lock emptied, content gone",
        &[Lag, PackLost, EmptyLock],
    ),
];

/// States with index runs gone or cut.
pub(super) const RUNS: &[(&str, &[Step])] = &[
    ("no index runs", &[NoRuns]),
    ("index runs cut", &[HalfRuns]),
    ("no index runs, unfinished pack record", &[NoRuns, Torn(0)]),
    (
        "no index runs, unfinished map.alt record",
        &[NoRuns, Torn(3)],
    ),
    ("index runs cut, unfinished operation", &[HalfRuns, Torn(4)]),
    (
        "no index runs, pack record past the boundary does not read",
        &[NoRuns, Lag, Damage(0)],
    ),
    (
        "index runs cut, map.alt record past the boundary does not read",
        &[HalfRuns, Lag, Damage(3)],
    ),
    ("no index runs, content gone", &[NoRuns, Lag, PackLost]),
    ("no index runs, never marked", &[NoRuns, NeverMarked]),
    (
        "no index runs, never marked, unfinished map.alt record",
        &[NoRuns, NeverMarked, Torn(3)],
    ),
];

/// States with a file cut to nothing or gone.
pub(super) const GONE: &[(&str, &[Step])] = &[
    ("operation log cut to nothing", &[EmptyLog]),
    ("no tier1 map", &[NoTier1]),
];

/// The repository every state is made of.
pub(super) struct Base {
    tmp: alt_os::temp::TempDir,
    /// how long the logs were after the first commit
    pub first: [u64; 5],
    /// the markers as they were then
    markers: [Vec<u8>; 2],
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap().map(|entry| entry.unwrap()) {
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// The index run directories there are.
pub(super) fn run_dirs(alt_dir: &Path) -> Vec<PathBuf> {
    [
        "store/chunks.idx",
        "store/manifests/blobmap.idx",
        "tier1.idx",
        "map.alt.idx",
    ]
    .iter()
    .map(|dir| alt_dir.join(dir))
    .filter(|dir| dir.is_dir())
    .collect()
}

impl Base {
    /// A first commit of enough files for the chunk index and `map.alt`
    /// to get index runs, then two more that add records to all five
    /// logs.
    pub fn build() -> Self {
        let tmp = alt_os::temp::tempdir().unwrap();
        let dir = tmp.path().join("base");
        fs::create_dir(&dir).unwrap();
        ok(alt(&dir, &["init"]));
        fs::write(dir.join("a.txt"), "one\n").unwrap();
        for n in 0..300 {
            fs::write(dir.join(format!("f{n}.txt")), format!("file {n}\n")).unwrap();
        }
        ok(alt(&dir, &["add", "."]));
        ok(alt(&dir, &["commit", "-m", "first"]));
        let first = lens(&dir);
        let markers = MARKERS.map(|marker| fs::read(dir.join(".alt").join(marker)).unwrap());
        for round in 1..=2u64 {
            fs::write(dir.join("a.txt"), format!("one\nround {round}\n")).unwrap();
            fs::write(dir.join(format!("l{round}.bin")), noise(round, 1 << 20)).unwrap();
            let zip = archive(&noise(round + 8, 4000));
            fs::write(dir.join(format!("z{round}.zip")), zip).unwrap();
            ok(alt(&dir, &["add", "."]));
            ok(alt(&dir, &["commit", "-m", &format!("round {round}")]));
        }
        for (log, (first, last)) in LOGS.iter().zip(first.iter().zip(lens(&dir))) {
            assert!(last > *first, "{log} holds nothing of the later commits");
        }
        let runs = run_dirs(&dir.join(".alt"));
        assert!(runs.len() >= 2, "the chunk index and map.alt have runs");
        Self {
            tmp,
            first,
            markers,
        }
    }

    /// A copy of the repository in the state `steps` make, under `name`.
    pub fn state(&self, name: &str, steps: &[Step]) -> PathBuf {
        let dir = self.tmp.path().join(name);
        copy_tree(&self.tmp.path().join("base"), &dir);
        self.change(&dir, steps);
        dir
    }

    /// Does `steps` to the repository in `dir`.
    pub fn change(&self, dir: &Path, steps: &[Step]) {
        for step in steps {
            self.apply(dir, *step);
        }
    }

    fn apply(&self, dir: &Path, step: Step) {
        let alt_dir = dir.join(".alt");
        let cut = |file: &str, len: u64| {
            let file = fs::OpenOptions::new().write(true).open(alt_dir.join(file));
            file.unwrap().set_len(len).unwrap();
        };
        match step {
            Torn(n) => {
                // the first bytes of a record of this log
                let mut bytes = fs::read(alt_dir.join(LOGS[n])).unwrap();
                let from = self.first[n] as usize;
                let begun = bytes[from..from + [44, 20, 20, 20, 30][n]].to_vec();
                bytes.extend(begun);
                fs::write(alt_dir.join(LOGS[n]), bytes).unwrap();
            }
            Lag => lag(dir, &self.markers),
            Damage(n) => damage(dir, n, self.first[n]),
            PackLost => cut(LOGS[0], self.first[0]),
            OlderMarkers => {
                let lens = lens(dir);
                let files = [lens[0], lens[1], lens[3]].map(u64::to_le_bytes).concat();
                fs::write(alt_dir.join(MARKERS[0]), files).unwrap();
                fs::write(alt_dir.join(MARKERS[1]), lens[4].to_le_bytes()).unwrap();
            }
            NeverMarked => {
                for marker in MARKERS {
                    fs::remove_file(alt_dir.join(marker)).unwrap();
                }
                cut("odb.lock", 0);
                cut("oplog/sync.lock", 0);
            }
            EmptyMarkers => MARKERS.iter().for_each(|marker| cut(marker, 0)),
            EmptyLock => cut("odb.lock", 0),
            NoRuns => run_dirs(&alt_dir)
                .iter()
                .for_each(|dir| fs::remove_dir_all(dir).unwrap()),
            HalfRuns => {
                for run in run_dirs(&alt_dir)
                    .iter()
                    .flat_map(|dir| fs::read_dir(dir).unwrap())
                {
                    let run = run.unwrap().path();
                    let len = fs::metadata(&run).unwrap().len();
                    fs::OpenOptions::new()
                        .write(true)
                        .open(&run)
                        .unwrap()
                        .set_len(len / 2)
                        .unwrap();
                }
            }
            EmptyLog => cut(LOGS[4], 0),
            NoTier1 => fs::remove_file(alt_dir.join(LOGS[2])).unwrap(),
        }
    }
}
