//! The working tree against the index, through the snapshot kept beside
//! the index: which paths are staged, changed, deleted or untracked,
//! without reading a file whose stat is the one last seen with its
//! staged content.

use std::cell::OnceCell;
use std::os::unix::fs::FileExt;

use alt_worktree::{Delta, PathMatcher, ScanOptions, Snapshot, Staged};

use super::*;

mod held;

use held::HELD;
pub use held::hold_snapshots;

/// A checkout worked out from the entries of a tree.
pub(super) struct Planned {
    /// the snapshot id of the index it ends with
    pub(super) index: String,
    /// the mode and the content snapshot it writes at each path
    pub(super) files: std::collections::HashMap<Vec<u8>, (u32, String)>,
}

/// What `status` and the commands that ask the same questions work from.
pub(super) struct TreeState {
    pub(super) snapshot: Snapshot,
    /// HEAD to index
    pub(super) staged: Vec<Staged>,
    /// index to working tree
    pub(super) delta: Delta,
}

impl NativeRepo<'_> {
    fn snapshot_path(&self) -> PathBuf {
        self.index_path.with_file_name("index.scan")
    }

    /// The snapshot of the index as it is now: the one on disk when it was
    /// built from this index, else a new one that takes over what the old
    /// one knows about unchanged entries.
    pub(super) fn worktree_snapshot(&self) -> Res<Snapshot> {
        let algo = self.store.algo;
        let path = self.snapshot_path();
        let prior = HELD.get(&path).or_else(|| {
            let read = Snapshot::load(&path, algo)?;
            HELD.put(&path, &read);
            Some(read)
        });
        if let Some(snapshot) = &prior
            && index_checksum(&self.index_path, algo.raw_len())?.as_ref()
                == Some(&snapshot.index_id)
        {
            return Ok(prior.expect("just matched"));
        }
        let bytes = match std::fs::read(&self.index_path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Snapshot::of_index(&empty_index(), &[], algo, None));
            }
            Err(e) => return Err(e.into()),
        };
        let index = Index::parse(&bytes, algo)?;
        let id = &bytes[bytes.len() - algo.raw_len()..];
        Ok(Snapshot::of_index(&index, id, algo, prior.as_ref()))
    }

    /// Keeps what a scan learned. The snapshot is a cache: where it cannot
    /// be written, the next scan reads the files again.
    pub(super) fn keep_snapshot(&self, snapshot: &Snapshot) {
        let path = self.snapshot_path();
        if snapshot.changed && snapshot.save(&path).is_ok() {
            HELD.put(&path, snapshot);
        }
    }

    /// Scans the working tree against `snapshot`, each file seen as it
    /// would be stored: an LFS path as its pointer, text with its line
    /// endings normalized.
    pub(super) fn scan_delta(&self, snapshot: &mut Snapshot, untracked: bool) -> Res<Delta> {
        Ok(self.scan_delta_within(snapshot, untracked, &[])?.0)
    }

    /// [`scan_delta`](Self::scan_delta) over `within` and what lies beneath
    /// those paths, the whole tree when there are none. Also returns the
    /// moment the scan began: a file changed before it can be recorded as
    /// unchanged.
    pub(super) fn scan_delta_within(
        &self,
        snapshot: &mut Snapshot,
        untracked: bool,
        within: &[&[u8]],
    ) -> Res<(Delta, alt_worktree::Clock)> {
        // built when the first file has to be read
        let tools: OnceCell<Res<(Option<PathMatcher>, convert::Converter)>> = OnceCell::new();
        let get = || {
            tools
                .get_or_init(|| Ok((self.lfs_rules()?, self.converter()?)))
                .as_ref()
                .ok()
        };
        let clean = |path: &[u8], content: &[u8], staged: ObjectId| {
            let (rules, attrs) = get()?;
            let pointer = rules
                .as_ref()
                .and_then(|r| lfs::lfs_clean(r, path, content));
            pointer.or_else(|| self.stored_form_of(attrs, Some(staged), path, content))
        };
        let head = |dir: &Path| get().and_then(|(_, attrs)| attrs.submodule_head(dir));
        let control = self.index_path.parent().ok_or("index has no directory")?;
        let clock = alt_worktree::Clock::read(control).unwrap_or(alt_worktree::Clock::UNREADABLE);
        let options = ScanOptions {
            untracked,
            within,
            clean: Some(&clean),
            head: Some(&head),
            clock,
        };
        let delta = alt_worktree::scan(&self.root, snapshot, &options)?;
        if let Some(tools) = tools.into_inner() {
            tools?.1.check()?;
        }
        Ok((delta, clock))
    }

    /// After a checkout put `index` in place: the snapshot of that index,
    /// with the stat of each file the checkout wrote. A file written in the
    /// current tick of the clock is left out and read by the next scan.
    pub(super) fn snapshot_checked_out(&self, index: &[u8]) -> Res<()> {
        let algo = self.store.algo;
        let mut written = std::mem::take(&mut *self.written.borrow_mut());
        // only where this is the index the planned checkout ends with do
        // the files it wrote hold what the entries name
        let planned = self.planned.borrow_mut().take();
        if !planned.is_some_and(|p| p.index == recovery::hash(index)) {
            written.clear();
        }
        let parsed = Index::parse(index, algo)?;
        let path = self.snapshot_path();
        let prior = HELD.get(&path).or_else(|| Snapshot::load(&path, algo));
        let id = &index[index.len() - algo.raw_len()..];
        let mut next = Snapshot::of_index(&parsed, id, algo, prior.as_ref());
        let control = self.index_path.parent().ok_or("index has no directory")?;
        let clock = alt_worktree::Clock::read(control).unwrap_or(alt_worktree::Clock::UNREADABLE);
        for (path, stat) in written {
            next.settle_path(&path, stat, &clock);
        }
        next.fill_trees();
        self.keep_snapshot(&next);
        Ok(())
    }

    /// The tree of the commit HEAD names; `None` when there is no commit yet.
    fn head_tree(&self) -> Res<Option<ObjectId>> {
        let Some(commit) = self.store.refs.resolve(&self.head_branch()?)? else {
            return Ok(None);
        };
        let obj = self
            .store
            .odb
            .get(&commit)?
            .ok_or("commit missing from store")?;
        Ok(Some(
            alt_git_codec::Commit::parse(&obj.data)?
                .tree()
                .ok_or("commit has no tree")?,
        ))
    }

    /// HEAD, index and working tree compared. `untracked` says whether
    /// files the index does not have are looked for.
    pub(super) fn tree_state(&self, untracked: bool) -> Res<TreeState> {
        let mut snapshot = self.worktree_snapshot()?;
        snapshot.fill_trees();
        let staged = snapshot.staged(&self.store.odb, self.head_tree()?)?;
        let delta = self.scan_delta(&mut snapshot, untracked)?;
        self.keep_snapshot(&snapshot);
        Ok(TreeState {
            snapshot,
            staged,
            delta,
        })
    }

    /// HEAD to index alone.
    pub(super) fn staged_changes(&self) -> Res<Vec<Staged>> {
        let mut snapshot = self.worktree_snapshot()?;
        snapshot.fill_trees();
        let staged = snapshot.staged(&self.store.odb, self.head_tree()?)?;
        self.keep_snapshot(&snapshot);
        Ok(staged)
    }
}

impl TreeState {
    /// The three lists `status` prints.
    pub(super) fn status(&self) -> alt_worktree::Status {
        let kind = |s: &Staged| match (&s.old, &s.new) {
            (None, _) => ChangeKind::Added,
            (_, None) => ChangeKind::Deleted,
            _ => ChangeKind::Modified,
        };
        let mut unstaged: Vec<(BString, ChangeKind)> = self
            .delta
            .changed
            .iter()
            .map(|w| (w.path.clone(), ChangeKind::Modified))
            .chain(
                self.delta
                    .deleted
                    .iter()
                    .map(|p| (p.clone(), ChangeKind::Deleted)),
            )
            .collect();
        unstaged.sort_by(|a, b| a.0.cmp(&b.0));
        alt_worktree::Status {
            staged: self
                .staged
                .iter()
                .map(|s| (s.path.clone(), kind(s)))
                .collect(),
            unstaged,
            untracked: self.delta.untracked.clone(),
        }
    }

    /// The paths with a conflict in the index.
    pub(super) fn unmerged(&self) -> std::collections::BTreeSet<BString> {
        let s = &self.snapshot;
        (0..s.entries.len())
            .filter(|&i| s.entries[i].stage() > 0)
            .map(|i| BString::from(s.path(i)))
            .collect()
    }
}

/// The checksum the index file at `path` ends in; `None` when there is no
/// index file.
pub(super) fn index_checksum(path: &Path, len: usize) -> Res<Option<Vec<u8>>> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let size = file.metadata()?.len();
    let mut id = vec![0; len];
    let at = size
        .checked_sub(len as u64)
        .ok_or("index file is truncated")?;
    file.read_exact_at(&mut id, at)?;
    Ok(Some(id))
}
