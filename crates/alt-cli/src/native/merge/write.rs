//! Writing the state a merge with conflicts leaves: the files and the
//! index.

use super::*;
use checkout_files::apart::Coming;
use checkout_recovery::{CheckoutIntent, FileChange, FileState};
use recovery::canonical_index;

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

    /// The resolutions among `resolved` that write something, refused
    /// where a file and a directory among them have one name to the file
    /// system.
    fn changed_apart<'r>(&self, resolved: &'r [Resolved], was: &Whole) -> Res<Vec<&'r Resolved>> {
        let changed: Vec<&Resolved> = resolved.iter().filter(|r| !untouched(r, was)).collect();
        let file = |r: &&&Resolved| r.worktree.is_some() || r.entry.is_some();
        let submodule = |r: &Resolved| r.entry.as_ref().is_some_and(|e| e.mode == 0o160000);
        let come = changed.iter().filter(file).map(|r| (&r.path, submodule(r)));
        let come: Vec<(&BString, bool)> = come.collect();
        let leave = changed
            .iter()
            .filter(|r| !file(r) && was.contains_key(&r.path));
        let leave = leave.map(|r| &r.path).collect();
        let held = was
            .keys()
            .chain(come.iter().map(|(path, _)| *path))
            .collect();
        self.ensure_none_respelled(&Coming { come, leave, held })?;
        Ok(changed)
    }

    /// Writes a conflicted merge state to disk: each resolution's working-tree
    /// bytes, then an index carrying stage-0 entries for clean paths and
    /// stage 1/2/3 entries for conflicted ones (so git tools see the merge).
    /// Written as a checkout is: the intent in the log first, with all it
    /// needs stored, so that one cut short is finished from the log.
    pub(in crate::native) fn write_merge_state(&mut self, resolved: &[Resolved]) -> Res<()> {
        // a path the merge leaves as it was keeps its file and its index
        // entry: only what changes is written
        let index = self.index()?.entries.into_iter();
        let was: Whole = index
            .filter(|e| e.stage() == 0)
            .map(|e| (e.path.clone(), e))
            .collect();
        let changed = self.changed_apart(resolved, &was)?;
        // refused before the first write, as a checkout is: over an
        // untracked file, or a directory that holds one. A directory that
        // holds only ignored files goes with them, as in git
        let tracked = |p: &[u8]| was.contains_key(&BString::from(p.to_vec()));
        let mut visible = checkout_plan::Visible::default();
        let mut ignored = Vec::new();
        for r in &changed {
            let file = r.worktree.is_some() || r.entry.as_ref().is_some_and(|e| e.mode != 0o160000);
            match self.abs(&r.path)?.symlink_metadata() {
                Ok(m) if m.is_dir() && file => {
                    ignored.extend(self.ignored_in_the_way(&r.path, &tracked, &mut visible)?);
                }
                Ok(m) if !m.is_dir() && !was.contains_key(&r.path) => {
                    let leaves = |r: &&&Resolved| {
                        !r.conflicted && r.entry.is_none() && was.contains_key(&r.path)
                    };
                    if self.respells(&r.path, changed.iter().filter(leaves).map(|r| &r.path))? {
                        continue;
                    }
                    let why = self.why_in_the_way(&r.path, was.keys())?;
                    return Err(crate::machine::Failure::refusal("precondition_failed", why).into());
                }
                _ => {}
            }
        }
        let changed: Vec<&BString> = changed.iter().map(|r| &r.path).collect();
        let mut intent = self.merged_checkout(resolved, &was, &changed)?;
        for path in ignored {
            intent.files.push(FileChange {
                before: self.file_state(&path, true)?,
                path: path.to_vec(),
                after: None,
            });
        }
        self.record_and_checkout(intent)?;
        // where a torn index file finds the index of the stop again
        let index = self.index()?;
        self.keep_stopped(index)?;
        // the state file planned for the stop holds with what was written,
        // or by itself where the merge left everything as it was
        self.settle_state()
    }

    /// What writing a merge that stopped changes, as the intent of a
    /// checkout: for each path the file that is there and the one to
    /// come, and the index with the stages of each conflict. Nothing is
    /// written to the working tree or the index.
    fn merged_checkout(
        &mut self,
        resolved: &[Resolved],
        was: &Whole,
        changed: &[&BString],
    ) -> Res<CheckoutIntent> {
        // the index to come names the blobs of the paths that merged
        self.store.odb.flush_synced()?;
        // converted under the attribute files of the merge result, as a
        // checkout of it would be: those on disk are from before the merge
        let files = self.merged_attribute_files(resolved)?;
        let files: Vec<(&[u8], &[u8])> = files.iter().map(|(d, c)| (*d, c.as_slice())).collect();
        let attrs = self.converter_under(&files)?;
        let among: std::collections::HashSet<&[u8]> =
            changed.iter().map(|path| path.as_slice()).collect();
        let mut changes = Vec::new();
        let mut planned = std::collections::HashMap::new();
        let mut entries = Vec::new();
        for r in resolved {
            if r.conflicted {
                entries.extend(r.stages.iter().map(|(stage, w)| stage_entry(w, *stage)));
            } else if kept(r, was) {
                entries.push(was[&r.path].clone());
            } else if let Some(e) = &r.entry {
                entries.push(stage_entry(e, 0));
            }
            if untouched(r, was) {
                continue;
            }
            let after = match (&r.worktree, &r.entry) {
                (Some(bytes), _) => {
                    let converted = attrs.to_worktree(&r.path, bytes)?;
                    let mode = if r.mode == 0o100755 { r.mode } else { 0o100644 };
                    let contents = self.snapshot(converted.as_deref().unwrap_or(bytes))?;
                    Some(FileState { mode, contents })
                }
                (None, Some(e)) => {
                    let state = self.target_state(e, &attrs)?;
                    planned.insert(r.path.to_vec(), (state.mode, state.contents.clone()));
                    Some(state)
                }
                (None, None) => None,
            };
            let modes = [
                was.get(&r.path).map(|e| e.mode),
                r.entry.as_ref().map(|e| e.mode),
            ];
            let gitlink = modes.contains(&Some(0o160000));
            let before = self.observed(&r.path, true, gitlink, &|p| among.contains(p))?;
            if before != after {
                let path = r.path.to_vec();
                changes.push(FileChange {
                    path,
                    before,
                    after,
                });
            }
        }
        let come = resolved
            .iter()
            .filter(|r| r.worktree.is_some() || r.entry.is_some());
        let come = come
            .map(|r| &r.path)
            .filter(|path| !was.contains_key(*path));
        self.keep_respelled(&mut changes, come)?;
        let algo = self.store.algo()?;
        let index = Index {
            version: 2,
            entries,
            extensions: Vec::new(),
        };
        let index_after = self.snapshot(&canonical_index(index, algo))?;
        *self.planned.borrow_mut() = Some(tree_state::Planned {
            index: index_after.clone(),
            files: planned,
        });
        Ok(CheckoutIntent {
            version: 1,
            workspace: self.workspace_identity()?,
            request_id: self.request_id.map(|id| crate::machine::hex(&id)),
            index_before: self.snapshot(&canonical_index(self.index()?, algo))?,
            index_after,
            files: changes,
        })
    }
}
