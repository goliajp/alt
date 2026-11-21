//! Writing the state a merge with conflicts leaves: the files and the
//! index.

use super::*;

/// The entries of the index that are in no conflict, by path.
type Whole = std::collections::HashMap<BString, IndexEntry>;

/// Whether a merge leaves the path of `r` as the index has it.
fn kept(r: &Resolved, was: &Whole) -> bool {
    match (&r.entry, was.get(&r.path)) {
        (Some(e), Some(was)) => !r.conflicted && e.oid == was.oid && e.mode == was.mode,
        _ => false,
    }
}

/// Whether a merge has nothing to write at the path of `r`: it leaves it
/// as it is, or has no file there where the index has none either.
fn untouched(r: &Resolved, was: &Whole) -> bool {
    let absent = r.entry.is_none() && !r.conflicted && !was.contains_key(&r.path);
    absent || kept(r, was)
}

impl NativeRepo<'_> {
    /// The attribute files among `resolved`, each with the directory it
    /// is in.
    fn merged_attribute_files<'a>(
        &self,
        resolved: &'a [Resolved],
    ) -> Res<Vec<(&'a [u8], Vec<u8>)>> {
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
    pub(in crate::native) fn write_merge_state(&mut self, resolved: &[Resolved]) -> Res<()> {
        // a path the merge leaves as it was keeps its file and its index
        // entry: only what changes is written
        let index = self.index()?.entries.into_iter();
        let was: Whole = index
            .filter(|e| e.stage() == 0)
            .map(|e| (e.path.clone(), e))
            .collect();
        let changed = resolved.iter().filter(|r| !untouched(r, &was));
        let changed: Vec<&BString> = changed.map(|r| &r.path).collect();
        // refused before the first write, as a checkout is
        for path in changed.iter().filter(|path| !was.contains_key(**path)) {
            if self
                .abs(path)?
                .symlink_metadata()
                .is_ok_and(|m| !m.is_dir())
            {
                let why = format!("untracked working tree file '{path}' would be overwritten");
                return Err(crate::machine::Failure::refusal("precondition_failed", why).into());
            }
        }
        let before = self.step_unrecorded(&changed)?;
        self.write_merged(resolved, &was)?;
        self.step_wrote(before)
    }

    fn write_merged(&mut self, resolved: &[Resolved], was: &Whole) -> Res<()> {
        // the index written below names the blobs of the paths that merged
        self.store.odb.flush_synced()?;
        // converted under the attribute files of the merge result, as a
        // checkout of it would be: those on disk are from before the merge
        let files = self.merged_attribute_files(resolved)?;
        let files: Vec<(&[u8], &[u8])> = files.iter().map(|(d, c)| (*d, c.as_slice())).collect();
        let attrs = self.converter_under(&files)?;
        for r in resolved {
            if untouched(r, was) {
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
                set_exec(&abs, r.mode == 0o100755)?;
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
            } else if kept(r, was) {
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
