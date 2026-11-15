//! When a scan began, by the clock that stamps the files it looks at, and
//! the rule that decides from it which stats may be recorded: a file found
//! unchanged is remembered by its stat only when a later change is certain
//! to move that stat.

use std::os::unix::fs::MetadataExt;
use std::path::Path;

use crate::snapshot::{FileStat, Snapshot};

/// A moment of a filesystem's clock.
pub type Stamp = (i64, u32);

/// A filesystem other than the one the clock was read on may stamp its
/// files with a coarser clock, or one that runs apart: its files are held
/// to a moment this many seconds earlier.
const APART_SECS: i64 = 3;

/// The moment a scan began.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Clock {
    /// read off a file written in the directory the snapshot lives in
    pub now: Stamp,
    /// the device that directory is on
    pub dev: u64,
}

impl Clock {
    /// For where the clock cannot be read: nothing is recorded as unchanged.
    pub const UNREADABLE: Clock = Clock {
        now: (i64::MIN, 0),
        dev: 0,
    };

    /// The current moment of the clock that stamps files in `control`.
    pub fn read(control: &Path) -> std::io::Result<Clock> {
        let probe = crate::control::clock_probe(control);
        std::fs::write(&probe, b"\n")?;
        let meta = std::fs::metadata(&probe)?;
        std::fs::remove_file(&probe)?;
        Ok(Clock {
            now: (meta.mtime(), meta.mtime_nsec() as u32),
            dev: meta.dev(),
        })
    }

    /// Whether a file with this stat was last changed before the scan
    /// began, so that any later change shows in its stat. The device is
    /// the file's own: a working tree on another filesystem than its
    /// snapshot, and a filesystem mounted somewhere inside the tree, are
    /// both held to the earlier moment.
    pub fn settled(&self, stat: &FileStat) -> bool {
        let limit = match stat.dev == self.dev {
            true => self.now,
            false => (self.now.0.saturating_sub(APART_SECS), self.now.1),
        };
        stat.mtime < limit && stat.ctime < limit
    }
}

impl Snapshot {
    /// Records `stat` as what the file of entry `at` looked like when it
    /// held the entry's content, unless a later change could leave that
    /// stat as it is; the file is then read again by the next scan.
    pub fn settle(&mut self, at: usize, stat: FileStat, clock: &Clock) {
        if clock.settled(&stat) {
            self.set_stat(at, Some(stat));
        }
    }

    /// [`settle`](Self::settle) for the entry at `path`, when there is one.
    pub fn settle_path(&mut self, path: &[u8], stat: FileStat, clock: &Clock) {
        if let Some(at) = self.find(path) {
            self.settle(at, stat, clock);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
    use alt_git_index::{Index, IndexEntry};

    const CLOCK: Clock = Clock {
        now: (1000, 500),
        dev: 7,
    };

    fn stat(dev: u64, mtime: Stamp, ctime: Stamp) -> FileStat {
        FileStat {
            mtime,
            ctime,
            dev,
            ino: 1,
            size: 1,
            mode: 0o100644,
        }
    }

    #[test]
    fn only_a_change_before_the_moment_is_settled() {
        let early = (1000, 499);
        assert!(CLOCK.settled(&stat(7, early, early)));
        assert!(CLOCK.settled(&stat(7, (999, 999_999_999), early)));
        // the tick the scan began in, and anything later
        assert!(!CLOCK.settled(&stat(7, (1000, 500), early)));
        assert!(!CLOCK.settled(&stat(7, early, (1000, 500))));
        assert!(!CLOCK.settled(&stat(7, (1001, 0), early)));
        assert!(!CLOCK.settled(&stat(7, early, (2000, 0))));
    }

    #[test]
    fn a_file_on_another_device_is_held_to_an_earlier_moment() {
        let near = (999, 0);
        assert!(CLOCK.settled(&stat(7, near, near)));
        assert!(!CLOCK.settled(&stat(8, near, near)));
        assert!(!CLOCK.settled(&stat(8, (997, 500), near)));
        assert!(!CLOCK.settled(&stat(8, near, (997, 500))));
        let far = (997, 499);
        assert!(CLOCK.settled(&stat(8, far, far)));
        assert!(CLOCK.settled(&stat(8, (0, 0), far)));
    }

    #[test]
    fn nothing_is_settled_where_the_clock_cannot_be_read() {
        let never = Clock::UNREADABLE;
        assert!(!never.settled(&stat(0, (i64::MIN, 0), (i64::MIN, 0))));
        assert!(!never.settled(&stat(9, (i64::MIN, 0), (i64::MIN, 0))));
        assert!(!never.settled(&stat(0, (0, 0), (0, 0))));
    }

    fn snapshot() -> Snapshot {
        let entries = ["a", "d/b"]
            .iter()
            .map(|path| IndexEntry {
                ctime: (0, 0),
                mtime: (0, 0),
                dev: 0,
                ino: 0,
                mode: 0o100644,
                uid: 0,
                gid: 0,
                size: 0,
                oid: ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, path.as_bytes()),
                flags: path.len() as u16,
                extended_flags: None,
                path: (*path).into(),
            })
            .collect();
        let index = Index {
            version: 2,
            entries,
            extensions: Vec::new(),
        };
        Snapshot::of_index(&index, b"id", HashAlgo::Sha1, None)
    }

    #[test]
    fn a_stat_is_recorded_only_when_settled() {
        let mut s = snapshot();
        let fresh = stat(7, (1000, 500), (1000, 500));
        s.settle(0, fresh, &CLOCK);
        s.settle_path(b"d/b", fresh, &CLOCK);
        assert!(s.entries.iter().all(|e| e.stat.is_none()));

        let old = stat(7, (900, 0), (900, 0));
        s.settle_path(b"d/b", old, &CLOCK);
        s.settle_path(b"not/there", old, &CLOCK);
        assert_eq!(s.entries[0].stat, None);
        assert_eq!(s.entries[1].stat, Some(old));
        s.settle(0, old, &CLOCK);
        assert_eq!(s.entries[0].stat, Some(old));
    }

    #[test]
    fn the_clock_read_lies_between_a_file_written_before_and_one_written_after() {
        let tmp = alt_os::temp::tempdir().unwrap();
        let dir = tmp.path();
        let of = |name: &str| FileStat::of(&std::fs::metadata(dir.join(name)).unwrap());
        std::fs::write(dir.join("before"), b"x").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(30));
        for round in 0..200 {
            let clock = Clock::read(dir).unwrap();
            std::fs::write(dir.join("after"), b"x").unwrap();
            assert!(clock.settled(&of("before")), "round {round}");
            assert!(!clock.settled(&of("after")), "round {round}");
            assert_eq!(clock.dev, of("after").dev);
        }
        // the probe is gone
        assert_eq!(std::fs::read_dir(dir).unwrap().count(), 2);
    }
}
