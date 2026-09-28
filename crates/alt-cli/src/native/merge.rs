//! The three-way merge engine: merge bases, and resolving two trees against
//! a base path by path.

use super::*;

impl NativeRepo<'_> {
    /// The best common ancestors of two commit sets: common ancestors that no
    /// other common ancestor descends from, in oid order so every run agrees.
    /// Several of them mean a criss-cross history; none, unrelated ones.
    pub(super) fn merge_bases(&self, a: &[ObjectId], b: &[ObjectId]) -> Res<Vec<ObjectId>> {
        let from_a = self.ancestry(a)?;
        let from_b = self.ancestry(b)?;
        let common: Vec<ObjectId> = from_a
            .keys()
            .filter(|c| from_b.contains_key(c))
            .copied()
            .collect();
        // a common commit is only a best one if no other common commit has it
        // as a proper ancestor, i.e. it is not below any common commit's parents
        let below: Vec<ObjectId> = common
            .iter()
            .flat_map(|c| from_a[c].iter().copied())
            .collect();
        let dominated = self.ancestry(&below)?;
        let mut bases: Vec<ObjectId> = common
            .into_iter()
            .filter(|c| !dominated.contains_key(c))
            .collect();
        bases.sort();
        Ok(bases)
    }

    /// Every commit reachable from `starts` (inclusive), each with its parents.
    fn ancestry(
        &self,
        starts: &[ObjectId],
    ) -> Res<std::collections::HashMap<ObjectId, Vec<ObjectId>>> {
        let mut seen = std::collections::HashMap::new();
        let mut stack = starts.to_vec();
        while let Some(c) = stack.pop() {
            if seen.contains_key(&c) {
                continue;
            }
            let obj = self.store.odb.get(&c)?.ok_or("commit missing from store")?;
            let parents: Vec<ObjectId> =
                alt_git_codec::Commit::parse(&obj.data)?.parents().collect();
            stack.extend(parents.iter().filter(|p| !seen.contains_key(*p)));
            seen.insert(c, parents);
        }
        Ok(seen)
    }

    /// The tree a merge is resolved against. One merge base is used as is. For
    /// a criss-cross, the bases are merged into each other in turn (each pair
    /// against its own merge bases, recursively) into a virtual base, as git
    /// does: picking one base instead lets the merge silently take whichever
    /// side that base happens to agree with. Conflicts inside the virtual base
    /// stay in it as content, so the real merge still sees both versions.
    /// Unrelated histories merge against an empty tree.
    pub(super) fn base_entries(&mut self, bases: &[ObjectId]) -> Res<Vec<WorkEntry>> {
        let Some((&first, rest)) = bases.split_first() else {
            return Ok(Vec::new());
        };
        let mut merged = vec![first];
        let mut acc = self.commit_entries(first)?;
        for &next in rest {
            let inner_bases = self.merge_bases(&merged, &[next])?;
            let inner_base = self.base_entries(&inner_bases)?;
            let theirs = self.commit_entries(next)?;
            let resolved =
                self.merge_trees(&inner_base, &acc, &theirs, "Temporary merge branch 2")?;
            acc = self.virtual_tree(resolved)?;
            merged.push(next);
        }
        Ok(acc)
    }

    /// Flattens a merge result into a tree, keeping each conflicted path's
    /// working-tree bytes (marker-laden text, or the side that still has the
    /// file) as its content.
    fn virtual_tree(&mut self, resolved: Vec<Resolved>) -> Res<Vec<WorkEntry>> {
        let mut out = Vec::with_capacity(resolved.len());
        for r in resolved {
            if !r.conflicted {
                out.extend(r.entry);
                continue;
            }
            let side = r
                .stages
                .iter()
                .find(|(stage, _)| *stage == 2)
                .or_else(|| r.stages.iter().find(|(stage, _)| *stage == 3))
                .map(|(_, e)| e.mode)
                .ok_or("conflict without either side")?;
            let bytes = r.worktree.ok_or("conflict without content")?;
            let oid = ObjectId::hash_object(self.store.algo, ObjectKind::Blob, &bytes);
            self.store.odb.put(oid, ObjectKind::Blob, &bytes)?;
            out.push(WorkEntry {
                path: r.path,
                oid,
                mode: side,
            });
        }
        Ok(out)
    }

    /// Three-way merges two trees over their common base, path by path.
    pub(super) fn merge_trees(
        &mut self,
        base: &[WorkEntry],
        ours: &[WorkEntry],
        theirs: &[WorkEntry],
        their_label: &str,
    ) -> Res<Vec<Resolved>> {
        use std::collections::{BTreeSet, HashMap};
        let map = |es: &[WorkEntry]| -> HashMap<BString, WorkEntry> {
            es.iter().map(|e| (e.path.clone(), e.clone())).collect()
        };
        let (bm, om, tm) = (map(base), map(ours), map(theirs));
        let paths: BTreeSet<BString> = bm
            .keys()
            .chain(om.keys())
            .chain(tm.keys())
            .cloned()
            .collect();

        let mut out = Vec::with_capacity(paths.len());
        for path in paths {
            let bo = bm.get(&path).cloned();
            let ao = om.get(&path).cloned();
            let to = tm.get(&path).cloned();
            out.push(self.resolve_path(path, bo, ao, to, their_label)?);
        }
        Ok(out)
    }

    /// Resolves one path's three-way state into a clean entry or a conflict.
    fn resolve_path(
        &mut self,
        path: BString,
        bo: Option<WorkEntry>,
        ao: Option<WorkEntry>,
        to: Option<WorkEntry>,
        their_label: &str,
    ) -> Res<Resolved> {
        let same = |x: &Option<WorkEntry>, y: &Option<WorkEntry>| match (x, y) {
            (None, None) => true,
            (Some(p), Some(q)) => p.oid == q.oid && p.mode == q.mode,
            _ => false,
        };
        if same(&ao, &to) {
            return Ok(Resolved::clean(path, ao)); // both agree (incl. both-deleted)
        }
        if same(&ao, &bo) {
            return Ok(Resolved::clean(path, to)); // ours unchanged → take theirs
        }
        if same(&to, &bo) {
            return Ok(Resolved::clean(path, ao)); // theirs unchanged → take ours
        }

        // both sides diverged from base
        match (&ao, &to) {
            (Some(a), Some(t)) => {
                let base_bytes = match &bo {
                    Some(b) => self.blob_bytes(b.oid)?,
                    None => Vec::new(),
                };
                let ours_bytes = self.blob_bytes(a.oid)?;
                let theirs_bytes = self.blob_bytes(t.oid)?;
                let unmergeable = a.mode != t.mode
                    || alt_diff::is_binary(&base_bytes)
                    || alt_diff::is_binary(&ours_bytes)
                    || alt_diff::is_binary(&theirs_bytes);
                if unmergeable {
                    // keep ours in the working tree, record all three stages
                    return Ok(make_conflict(path, bo, ao, to, ours_bytes));
                }
                let labels = alt_merge::Labels {
                    ours: "HEAD",
                    theirs: their_label,
                };
                let m = alt_merge::merge(&base_bytes, &ours_bytes, &theirs_bytes, &labels);
                if m.is_clean() {
                    let oid = ObjectId::hash_object(self.store.algo, ObjectKind::Blob, &m.content);
                    self.store.odb.put(oid, ObjectKind::Blob, &m.content)?;
                    Ok(Resolved::clean(
                        path.clone(),
                        Some(WorkEntry {
                            path,
                            oid,
                            mode: a.mode,
                        }),
                    ))
                } else {
                    Ok(make_conflict(path, bo, ao, to, m.content))
                }
            }
            // modify/delete: one side changed the file, the other removed it
            (Some(a), None) => {
                let bytes = self.blob_bytes(a.oid)?;
                Ok(make_conflict(path, bo, ao, to, bytes))
            }
            (None, Some(t)) => {
                let bytes = self.blob_bytes(t.oid)?;
                Ok(make_conflict(path, bo, ao, to, bytes))
            }
            (None, None) => unreachable!("same(ao, to) already handled both-None"),
        }
    }

    /// Where an unfinished merge records the commit being merged in, next to
    /// this workspace's index, so each workspace has its own.
    fn merge_head_path(&self) -> PathBuf {
        self.index_path.with_file_name("MERGE_HEAD")
    }

    /// The commit an unfinished (conflicted) merge is bringing in, if any.
    pub(super) fn merge_head(&self) -> Res<Option<ObjectId>> {
        match std::fs::read_to_string(self.merge_head_path()) {
            Ok(s) => Ok(Some(s.trim().parse().map_err(|_| "corrupt MERGE_HEAD")?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Records `theirs` as the second parent the resolving commit will get.
    pub(super) fn start_merge(&self, theirs: ObjectId) -> Res<()> {
        std::fs::write(self.merge_head_path(), format!("{theirs}\n"))?;
        Ok(())
    }

    /// Forgets the unfinished merge once its commit is recorded.
    pub(super) fn finish_merge(&self) -> Res<()> {
        match std::fs::remove_file(self.merge_head_path()) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }

    /// Refuses `verb` while a merge is waiting for its resolving commit.
    pub(super) fn ensure_no_merge_in_progress(&self, verb: &str) -> Res<()> {
        if self.merge_head()?.is_some() {
            return Err(format!(
                "{verb}: a merge is in progress; resolve the conflicts and commit it first"
            )
            .into());
        }
        Ok(())
    }
}
