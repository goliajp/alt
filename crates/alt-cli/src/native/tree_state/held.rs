//! Snapshots kept in memory by a process that serves many commands.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use alt_worktree::{FileStat, Snapshot};

/// A snapshot with what its file looked like when it was read or written.
/// Another process that replaces the file makes the copy stale; a stale
/// copy only knows less, since every entry still says which content its
/// stat goes with.
struct Held {
    file: FileStat,
    snapshot: Snapshot,
}

/// The snapshots a process holds, by the path of their file; holds
/// nothing until it is told to.
pub(super) struct Shelf(Mutex<Option<HashMap<PathBuf, Held>>>);

pub(super) static HELD: Shelf = Shelf::new();

/// From now on this process keeps the snapshots it reads and writes, and
/// reads a snapshot file again only after it was replaced. For the daemon.
pub fn hold_snapshots() {
    HELD.start();
}

impl Shelf {
    pub(super) const fn new() -> Shelf {
        Shelf(Mutex::new(None))
    }

    pub(super) fn start(&self) {
        self.0.lock().unwrap().get_or_insert_default();
    }

    /// The snapshot held for the file at `path`, when the file is still
    /// the one it was taken from or written to.
    pub(super) fn get(&self, path: &Path) -> Option<Snapshot> {
        let guard = self.0.lock().unwrap();
        let held = guard.as_ref()?.get(path)?;
        let now = FileStat::of(&std::fs::symlink_metadata(path).ok()?);
        (now == held.file).then(|| held.snapshot.clone())
    }

    /// Holds `snapshot` as what the file at `path` says right now.
    pub(super) fn put(&self, path: &Path, snapshot: &Snapshot) {
        let mut guard = self.0.lock().unwrap();
        let Some(map) = guard.as_mut() else { return };
        let Ok(meta) = std::fs::symlink_metadata(path) else {
            return;
        };
        let mut snapshot = snapshot.clone();
        snapshot.changed = false;
        let file = FileStat::of(&meta);
        map.insert(path.to_path_buf(), Held { file, snapshot });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alt_git_codec::HashAlgo;
    use alt_git_index::Index;

    fn snapshot(id: &[u8]) -> Snapshot {
        let index = Index {
            version: 2,
            entries: Vec::new(),
            extensions: Vec::new(),
        };
        Snapshot::of_index(&index, id, HashAlgo::Sha1, None)
    }

    #[test]
    fn a_process_that_was_not_told_to_holds_nothing() {
        let dir = alt_os::temp::tempdir().unwrap();
        let path = dir.path().join("index.scan");
        let first = snapshot(b"first");
        first.save(&path).unwrap();
        let shelf = Shelf::new();
        shelf.put(&path, &first);
        assert!(shelf.get(&path).is_none());
    }

    #[test]
    fn a_snapshot_is_held_until_its_file_is_replaced() {
        let dir = alt_os::temp::tempdir().unwrap();
        let path = dir.path().join("index.scan");
        let shelf = Shelf::new();
        shelf.start();
        assert!(shelf.get(&path).is_none());

        let first = snapshot(b"first");
        // nothing is held for a file that is not there
        shelf.put(&path, &first);
        assert!(shelf.get(&path).is_none());
        first.save(&path).unwrap();
        shelf.put(&path, &first);
        let held = shelf.get(&path).unwrap();
        assert_eq!(held.index_id, b"first");
        assert!(!held.changed, "what is held is what the file says");
        assert!(shelf.get(&dir.path().join("other.scan")).is_none());

        // another process writes the file anew: the same bytes even, but
        // not the file the copy was taken from
        first.save(&path).unwrap();
        assert!(shelf.get(&path).is_none());
        let second = snapshot(b"second");
        second.save(&path).unwrap();
        assert!(shelf.get(&path).is_none());
        shelf.put(&path, &second);
        assert_eq!(shelf.get(&path).unwrap().index_id, b"second");

        std::fs::remove_file(&path).unwrap();
        assert!(shelf.get(&path).is_none());
    }
}
