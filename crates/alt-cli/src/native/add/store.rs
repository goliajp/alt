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
    /// `scanned` is the converter the scan read the files with, if it
    /// read any: stored with the same one, a file is filtered once.
    pub(super) fn stage_files(
        &mut self,
        taken: &[&BString],
        delta: &Delta,
        before: &Index,
        entries: &mut Vec<IndexEntry>,
        scanned: Option<convert::Converter>,
    ) -> Res<Stored> {
        let lfs = self.lfs_rules()?;
        let attrs = match scanned {
            Some(attrs) => attrs,
            None => self.converter()?,
        };
        let moved: HashMap<&[u8], &WorkEntry> = delta
            .changed
            .iter()
            .filter(|w| w.mode == 0o160000)
            .map(|w| (w.path.as_slice(), w))
            .collect();
        let (mut stats, mut blocked) = (Vec::new(), Vec::new());
        let modes = self.staged_modes()?;
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
            let inner = meta
                .is_dir()
                .then(|| attrs.submodule_head(&self.abs(path).ok()?));
            if let Some(Some(commit)) = inner {
                entries.push(staged_entry(path, commit, 0o160000));
                continue;
            }
            if meta.is_dir() {
                blocked.push(no_commit(path)?);
                continue;
            }
            let stat = FileStat::of(&meta);
            let mode = modes.of(path, stat.git_mode());
            entries.push(self.stored(path, mode, lfs.as_ref(), &attrs, before)?);
            stats.push((path.clone(), stat));
        }
        Ok(Stored { stats, blocked })
    }

    /// Stores the file at `path` as it is cleaned, and returns the entry
    /// the index gets for it.
    pub(in crate::native) fn stored(
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
            oid: ObjectId::hash_object(self.store.algo()?, ObjectKind::Blob, b""),
            mode,
        };
        let bytes = self.staged_bytes(&file, lfs, attrs, before)?;
        let oid = ObjectId::hash_object(self.store.algo()?, ObjectKind::Blob, &bytes);
        self.store.odb.put(oid, ObjectKind::Blob, &bytes)?;
        Ok(staged_entry(path, oid, mode))
    }
}

/// Says, as git does, that the directory at `path` is a repository with
/// no commit, and gives the path back as one that was not stored.
fn no_commit(path: &BString) -> Res<BString> {
    let why = [
        b"error: '",
        &path[..],
        b"' does not have a commit checked out\n",
    ];
    crate::diagnostics::emit(&why.concat())?;
    Ok(path.clone())
}

/// The mode a file is stored with. Where `core.fileMode` is off, whether
/// a file can be executed is not told from the file: one the index has
/// keeps the mode it has there, and a new one is not executable.
pub(in crate::native) struct StagedModes {
    /// `None` where the mode found is the mode stored
    staged: Option<HashMap<BString, u32>>,
}

impl StagedModes {
    pub(in crate::native) fn of(&self, path: &BString, found: u32) -> u32 {
        let regular = |mode: u32| mode & 0o170000 == 0o100000;
        match &self.staged {
            Some(staged) if regular(found) => staged.get(path).copied().unwrap_or(0o100644),
            _ => found,
        }
    }
}

impl NativeRepo<'_> {
    pub(in crate::native) fn staged_modes(&self) -> Res<StagedModes> {
        if crate::git_settings::file_mode(&self.settings()?)? {
            return Ok(StagedModes { staged: None });
        }
        let regular = |e: &&IndexEntry| e.mode & 0o170000 == 0o100000;
        let index = self.index()?;
        let staged = index.entries.iter().filter(regular);
        let staged = staged.map(|e| (e.path.clone(), e.mode)).collect();
        Ok(StagedModes {
            staged: Some(staged),
        })
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
