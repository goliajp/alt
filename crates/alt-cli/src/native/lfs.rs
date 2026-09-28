//! Git LFS on the native store, the way git-lfs works: commits keep the
//! pointer files (so ids match the original git+LFS repository), the large
//! contents live in the object store under their sha256, the working tree
//! holds the real files, and paths `.gitattributes` marks `filter=lfs` are
//! turned back into pointers whenever the tree is scanned or staged.

use alt_worktree::{scan_indexed_paths, scan_worktree_with_index};

use alt_lfs::Pointer;
use alt_odb::BlobId;
use alt_worktree::{Clean, PathMatcher};

use super::*;

impl NativeRepo<'_> {
    /// The paths the root `.gitattributes` sends through LFS, if any.
    pub(super) fn lfs_rules(&self) -> Res<Option<PathMatcher>> {
        let text = match std::fs::read_to_string(self.root.join(".gitattributes")) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let patterns: Vec<&str> = text
            .lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .filter_map(|l| {
                let mut words = l.split_whitespace();
                let pattern = words.next()?;
                words.any(|w| w == "filter=lfs").then_some(pattern)
            })
            .collect();
        Ok((!patterns.is_empty()).then(|| PathMatcher::new(patterns)))
    }

    /// Scans the working tree against `index`, LFS paths seen as pointers.
    pub(super) fn scan_tree(&self, index: &Index) -> Res<Vec<WorkEntry>> {
        let rules = self.lfs_rules()?;
        let clean = |path: &[u8], content: &[u8]| lfs_clean(rules.as_ref()?, path, content);
        let filter: Option<Clean<'_>> = rules.is_some().then_some(&clean);
        Ok(scan_worktree_with_index(
            &self.root,
            index,
            self.store.algo,
            filter,
        )?)
    }

    /// Like [`scan_tree`](Self::scan_tree) but only over the index's paths.
    pub(super) fn scan_index_paths(&self, index: &Index) -> Res<Vec<WorkEntry>> {
        let rules = self.lfs_rules()?;
        let clean = |path: &[u8], content: &[u8]| lfs_clean(rules.as_ref()?, path, content);
        let filter: Option<Clean<'_>> = rules.is_some().then_some(&clean);
        Ok(scan_indexed_paths(
            &self.root,
            index,
            self.store.algo,
            filter,
        )?)
    }

    /// The bytes to store for a working-tree file: for an LFS path its
    /// pointer, with the content itself kept in the LFS store.
    pub(super) fn staged_bytes(
        &mut self,
        w: &WorkEntry,
        rules: Option<&PathMatcher>,
    ) -> Res<Vec<u8>> {
        let content = self.read_for(w)?;
        let rules = rules.filter(|_| w.mode != 0o120000);
        match rules.and_then(|r| lfs_clean(r, &w.path, &content)) {
            Some(pointer) => {
                self.lfs_store(&content)?;
                Ok(pointer)
            }
            None => Ok(content),
        }
    }

    /// The content a pointer names, when the store has it.
    pub(super) fn lfs_content(&self, p: &Pointer) -> Res<Option<Vec<u8>>> {
        match self.lfs_blob(&p.oid)? {
            Some(id) => Ok(self.store.odb.get_content(id)?),
            None => Ok(None),
        }
    }

    /// Keeps `content` in the store, outside the git object map so it never
    /// leaves as a git object, and found by its sha256 through
    /// `.alt/lfs/<aa>/<rest>`, which holds its blob id.
    pub(super) fn lfs_store(&mut self, content: &[u8]) -> Res<Pointer> {
        let pointer = Pointer::of(content);
        let link = self.lfs_link(&pointer.oid);
        if link.exists() {
            return Ok(pointer);
        }
        let id = self.store.odb.put_content(content)?;
        self.store.odb.flush()?;
        std::fs::create_dir_all(link.parent().expect("under .alt/lfs"))?;
        // the content is durable before the link names it
        let tmp = link.with_extension(format!("tmp{}", std::process::id()));
        std::fs::write(&tmp, id.0)?;
        std::fs::rename(&tmp, &link)?;
        Ok(pointer)
    }

    fn lfs_blob(&self, sha: &str) -> Res<Option<BlobId>> {
        match std::fs::read(self.lfs_link(sha)) {
            Ok(b) => Ok(Some(BlobId(b.try_into().map_err(|_| "corrupt LFS link")?))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Pointers are parsed strictly, so `sha` is 64 lowercase hex digits.
    fn lfs_link(&self, sha: &str) -> PathBuf {
        self.store
            .alt_dir
            .join("lfs")
            .join(&sha[..2])
            .join(&sha[2..])
    }

    /// The pointers HEAD's tree holds, with their paths.
    fn head_pointers(&self) -> Res<Vec<(BString, Pointer)>> {
        let mut out = Vec::new();
        for e in self.head_entries()? {
            if e.mode == 0o120000 || e.mode == 0o160000 {
                continue;
            }
            let Some(obj) = self.store.odb.get(&e.oid)? else {
                continue;
            };
            if let Some(p) = Pointer::parse(&obj.data) {
                out.push((e.path, p));
            }
        }
        Ok(out)
    }

    /// Replaces pointer files in the working tree with the contents the store
    /// now has, re-recording their stat so they read as unchanged.
    fn smudge_head(&mut self) -> Res<usize> {
        let mut written = 0;
        let pointers = self.head_pointers()?;
        let mut index = self.index()?;
        for (path, p) in pointers {
            let Some(content) = self.lfs_content(&p)? else {
                continue;
            };
            let abs = self.abs(&path)?;
            if std::fs::read(&abs).ok().as_deref() != Some(&p.encode()[..]) {
                continue; // edited or already real: leave it alone
            }
            std::fs::write(&abs, &content)?;
            if let Some(e) = index
                .entries
                .iter_mut()
                .find(|e| e.path == path && e.stage() == 0)
            {
                let meta = std::fs::symlink_metadata(&abs)?;
                *e = stat_entry(
                    &meta,
                    &WorkEntry {
                        path: path.clone(),
                        oid: e.oid,
                        mode: e.mode,
                    },
                );
            }
            written += 1;
        }
        save_index(&self.index_path, &index, self.store.algo)?;
        Ok(written)
    }

    /// `alt lfs fetch [<remote>]`: download the contents HEAD's pointers name
    /// that the store lacks, then put the real files in the working tree.
    pub fn lfs_fetch(&mut self, remote: &str, out: &mut impl Write) -> Res<()> {
        let mut missing: Vec<Pointer> = self
            .head_pointers()?
            .into_iter()
            .map(|(_, p)| p)
            .filter(|p| !self.lfs_link(&p.oid).exists())
            .collect();
        missing.sort();
        missing.dedup();
        if !missing.is_empty() {
            let remotes = self.read_remotes()?;
            let r = remotes
                .iter()
                .find(|r| r.name == remote)
                .ok_or_else(|| format!("no such remote '{remote}'"))?;
            let url = alt_repo::Repository::discover(&self.store.alt_dir)?.rewrite_url(&r.url);
            let auth = http_auth(remote, &url).map(|a| (a.username, a.token));
            let client = alt_lfs::Client::new(alt_lfs::endpoint_for(&url), auth);
            for (_, content) in client.download(&missing)? {
                self.lfs_store(&content)?;
            }
        }
        let written = self.smudge_head()?;
        writeln!(
            out,
            "fetched {} LFS object(s); {written} file(s) checked out",
            missing.len()
        )?;
        Ok(())
    }

    /// `alt lfs import <git-dir>`: take LFS contents from a git repository's
    /// local LFS storage and check them out.
    pub fn lfs_import(&mut self, git_dir: &Path, out: &mut impl Write) -> Res<()> {
        let taken = self.lfs_take(git_dir)?;
        let written = self.smudge_head()?;
        writeln!(
            out,
            "took {taken} LFS object(s) from {}; {written} file(s) checked out",
            git_dir.display()
        )?;
        Ok(())
    }

    /// Stores the contents HEAD's pointers name that `<git-dir>/lfs/objects`
    /// holds, where git-lfs keeps them.
    pub(super) fn lfs_take(&mut self, git_dir: &Path) -> Res<usize> {
        let mut taken = 0;
        for (_, p) in self.head_pointers()? {
            if self.lfs_link(&p.oid).exists() {
                continue;
            }
            let local = git_dir
                .join("lfs/objects")
                .join(&p.oid[..2])
                .join(&p.oid[2..4])
                .join(&p.oid);
            match std::fs::read(&local) {
                Ok(content) => {
                    p.verify(&content)?;
                    self.lfs_store(&content)?;
                    taken += 1;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(taken)
    }

    /// `alt lfs ls`: each pointer in HEAD, and whether its content is here.
    pub fn lfs_ls(&self, out: &mut impl Write) -> Res<()> {
        for (path, p) in self.head_pointers()? {
            let mark = if self.lfs_link(&p.oid).exists() {
                '*'
            } else {
                '-'
            };
            writeln!(out, "{} {mark} {path}", &p.oid[..10])?;
        }
        Ok(())
    }
}

/// git-lfs's clean step: an LFS path's content becomes its pointer; content
/// that already is a pointer stays as it is.
fn lfs_clean(rules: &PathMatcher, path: &[u8], content: &[u8]) -> Option<Vec<u8>> {
    if !rules.matches(path) || Pointer::parse(content).is_some() {
        return None;
    }
    Some(Pointer::of(content).encode())
}
