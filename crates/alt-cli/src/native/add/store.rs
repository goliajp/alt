//! The files an `add` stores, and the entries the index gets for them.

use std::collections::HashMap;

use alt_worktree::{Delta, FileStat, PathMatcher};

use super::*;

/// What became of the files an `add` was to store.
pub(super) struct Stored {
    /// the stat each file stored had before it was read
    pub(super) stats: Vec<(BString, FileStat)>,
    /// the paths that could not be stored
    pub(super) blocked: Vec<BString>,
}

impl NativeRepo<'_> {
    /// Stores what the working tree has at each of `taken` and pushes its
    /// entry. A directory where a tracked file is to be stored again is
    /// to git a repository with no commit, which it says and goes on.
    pub(super) fn stage_files(
        &mut self,
        taken: &[&BString],
        delta: &Delta,
        before: &Index,
        entries: &mut Vec<IndexEntry>,
    ) -> Res<Stored> {
        let lfs = self.lfs_rules()?;
        let attrs = self.converter()?;
        let moved: HashMap<&[u8], &WorkEntry> = delta
            .changed
            .iter()
            .filter(|w| w.mode == 0o160000)
            .map(|w| (w.path.as_slice(), w))
            .collect();
        let (mut stats, mut blocked) = (Vec::new(), Vec::new());
        for &path in taken {
            self.ensure_path_allowed(path)?;
            // a submodule is recorded by the commit it has checked out,
            // which lives in its own store
            if let Some(w) = moved.get(path.as_slice()) {
                entries.push(staged_entry(path, w.oid, w.mode));
                continue;
            }
            let gone =
                |e| settings::fatal(format!("unable to stat '{path}': {}", settings::reason(&e)));
            let meta = std::fs::symlink_metadata(self.abs(path)?).map_err(gone)?;
            if meta.is_dir() {
                let why = [
                    b"error: '",
                    &path[..],
                    b"' does not have a commit checked out\n",
                ];
                crate::diagnostics::emit(&why.concat())?;
                blocked.push(path.clone());
                continue;
            }
            let stat = FileStat::of(&meta);
            entries.push(self.stored(path, stat.git_mode(), lfs.as_ref(), &attrs, before)?);
            stats.push((path.clone(), stat));
        }
        Ok(Stored { stats, blocked })
    }

    /// Stores the file at `path` as it is cleaned, and returns the entry
    /// the index gets for it.
    fn stored(
        &mut self,
        path: &BString,
        mode: u32,
        lfs: Option<&PathMatcher>,
        attrs: &convert::Converter,
        before: &Index,
    ) -> Res<IndexEntry> {
        // the id is known once the stored form is: not used until then
        let file = WorkEntry {
            path: path.clone(),
            oid: ObjectId::hash_object(self.store.algo, ObjectKind::Blob, b""),
            mode,
        };
        let bytes = self.staged_bytes(&file, lfs, attrs, before)?;
        let oid = ObjectId::hash_object(self.store.algo, ObjectKind::Blob, &bytes);
        self.store.odb.put(oid, ObjectKind::Blob, &bytes)?;
        Ok(staged_entry(path, oid, mode))
    }
}

/// An index entry for staged content. The index keeps no stat: what the
/// working-tree file looked like is the snapshot's to remember.
fn staged_entry(path: &BString, oid: ObjectId, mode: u32) -> IndexEntry {
    IndexEntry {
        ctime: (0, 0),
        mtime: (0, 0),
        dev: 0,
        ino: 0,
        mode,
        uid: 0,
        gid: 0,
        size: 0,
        oid,
        flags: path.len().min(0x0FFF) as u16,
        extended_flags: None,
        path: path.clone(),
    }
}
