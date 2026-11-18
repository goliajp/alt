//! Writing the state a merge with conflicts leaves: the files and the
//! index.

use super::*;

impl NativeRepo<'_> {
    /// The attribute files among `resolved`, each with the directory it
    /// is in.
    fn attribute_files<'a>(&self, resolved: &'a [Resolved]) -> Res<Vec<(&'a [u8], Vec<u8>)>> {
        let mut files: Vec<(&[u8], Vec<u8>)> = Vec::new();
        for r in resolved {
            let dir = match r.path.as_slice() {
                b".gitattributes" => &b""[..],
                p => match p.strip_suffix(b"/.gitattributes") {
                    Some(dir) => dir,
                    None => continue,
                },
            };
            match (&r.worktree, &r.entry) {
                (Some(bytes), _) => files.push((dir, bytes.clone())),
                (None, Some(e)) if e.mode & 0o170000 == 0o100000 => {
                    files.push((dir, self.blob_bytes(e.oid)?));
                }
                _ => {}
            }
        }
        Ok(files)
    }

    /// Writes a conflicted merge state to disk: each resolution's working-tree
    /// bytes, then an index carrying stage-0 entries for clean paths and
    /// stage 1/2/3 entries for conflicted ones (so git tools see the merge).
    pub(in crate::native) fn write_conflicted(&mut self, resolved: &[Resolved]) -> Res<()> {
        // the index written below names the blobs of the paths that merged
        self.store.odb.flush_synced()?;
        // converted under the attribute files of the merge result, as a
        // checkout of it would be: those on disk are from before the merge
        let files = self.attribute_files(resolved)?;
        let files: Vec<(&[u8], &[u8])> = files.iter().map(|(d, c)| (*d, c.as_slice())).collect();
        let attrs = self.converter_under(&files)?;
        // a path the merge leaves as it was keeps its file and its index
        // entry: only what changes is written
        let was: std::collections::HashMap<BString, IndexEntry> = self
            .index()?
            .entries
            .into_iter()
            .filter(|e| e.stage() == 0)
            .map(|e| (e.path.clone(), e))
            .collect();
        let kept = |r: &Resolved| match (&r.entry, was.get(&r.path)) {
            (Some(e), Some(was)) => !r.conflicted && e.oid == was.oid && e.mode == was.mode,
            _ => false,
        };
        for r in resolved {
            if kept(r) {
                continue;
            }
            let abs = self.abs(&r.path)?;
            if let Some(bytes) = &r.worktree {
                if let Some(parent) = abs.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                if abs.symlink_metadata().is_ok() {
                    crate::durable::remove_file(&abs)?;
                }
                let converted = attrs.to_worktree(&r.path, bytes)?;
                crate::durable::write(&abs, converted.as_deref().unwrap_or(bytes))?;
            } else {
                match &r.entry {
                    Some(e) => self.materialize(e, &attrs)?,
                    None => {
                        if abs.symlink_metadata().is_ok() {
                            crate::durable::remove_file(&abs)?;
                            self.prune_empty_dirs(abs.parent());
                        }
                    }
                }
            }
        }

        let mut entries = Vec::new();
        for r in resolved {
            if r.conflicted {
                for (stage, w) in &r.stages {
                    entries.push(stage_entry(w, *stage));
                }
            } else if kept(r) {
                entries.push(was[&r.path].clone());
            } else if let Some(e) = &r.entry {
                entries.push(self.make_entry(e)?);
            }
        }
        save_index(
            &self.index_path,
            &Index {
                version: 2,
                entries,
                extensions: Vec::new(),
            },
            self.store.algo()?,
        )
    }
}
