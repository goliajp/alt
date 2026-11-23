//! The three-way merge engine: merge bases, and resolving two trees against
//! a base path by path.

use super::*;

mod reset;
mod run;
mod state;
pub use run::FastForward;
pub(super) use run::MergeOutcome;
mod write;

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
    pub(super) fn ancestry(
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
            let bytes = r.worktree.ok_or("conflict without content")?;
            let oid = self.store.put_blob(&bytes)?;
            out.push(WorkEntry {
                path: r.path,
                oid,
                mode: r.mode,
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
                let mode = merged_mode(bo.as_ref().map(|b| b.mode), a.mode, t.mode);
                let binary = alt_diff::is_binary(&base_bytes)
                    || alt_diff::is_binary(&ours_bytes)
                    || alt_diff::is_binary(&theirs_bytes);
                let (Some(mode), false) = (mode, binary) else {
                    // keep ours in the working tree, record all three stages
                    let mode = mode.unwrap_or(a.mode);
                    return Ok(make_conflict(path, bo, ao, to, ours_bytes, mode));
                };
                let labels = alt_merge::Labels {
                    ours: "HEAD",
                    theirs: their_label,
                };
                let m = alt_merge::merge(&base_bytes, &ours_bytes, &theirs_bytes, &labels);
                if m.is_clean() {
                    let oid = self.store.put_blob(&m.content)?;
                    Ok(Resolved::clean(
                        path.clone(),
                        Some(WorkEntry { path, oid, mode }),
                    ))
                } else {
                    Ok(make_conflict(path, bo, ao, to, m.content, mode))
                }
            }
            // modify/delete: one side changed the file, the other removed it
            (Some(a), None) => {
                let (bytes, mode) = (self.blob_bytes(a.oid)?, a.mode);
                Ok(make_conflict(path, bo, ao, to, bytes, mode))
            }
            (None, Some(t)) => {
                let (bytes, mode) = (self.blob_bytes(t.oid)?, t.mode);
                Ok(make_conflict(path, bo, ao, to, bytes, mode))
            }
            (None, None) => unreachable!("same(ao, to) already handled both-None"),
        }
    }
}

/// One path's merge resolution.
pub(super) struct Resolved {
    pub(super) path: BString,
    /// The clean result entry (`None` = deleted); meaningless when conflicted.
    pub(super) entry: Option<WorkEntry>,
    pub(super) conflicted: bool,
    /// Bytes to write to the working tree on conflict (markers or the kept
    /// side); `None` for a clean resolution.
    pub(super) worktree: Option<Vec<u8>>,
    /// The mode of the file those bytes are written as.
    pub(super) mode: u32,
    /// Unmerged index entries `(stage, entry)` for a conflict.
    pub(super) stages: Vec<(u8, WorkEntry)>,
}

impl Resolved {
    pub(super) fn clean(path: BString, entry: Option<WorkEntry>) -> Self {
        Resolved {
            path,
            entry,
            conflicted: false,
            worktree: None,
            mode: 0,
            stages: Vec::new(),
        }
    }
}

/// The mode of a file both sides have: the one they agree on, or the one a
/// single side changed from the base. `None` when each side has its own.
fn merged_mode(base: Option<u32>, ours: u32, theirs: u32) -> Option<u32> {
    let regular = |mode: u32| mode & 0o170000 == 0o100000;
    match base {
        _ if ours == theirs => Some(ours),
        Some(base) if regular(base) && regular(ours) && regular(theirs) => {
            if ours == base {
                Some(theirs)
            } else if theirs == base {
                Some(ours)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Builds a conflicted resolution: working-tree `bytes` written with `mode`,
/// plus stage 1/2/3 index entries for whichever of base/ours/theirs are
/// present.
pub(super) fn make_conflict(
    path: BString,
    bo: Option<WorkEntry>,
    ao: Option<WorkEntry>,
    to: Option<WorkEntry>,
    bytes: Vec<u8>,
    mode: u32,
) -> Resolved {
    let mut stages = Vec::new();
    if let Some(b) = bo {
        stages.push((1u8, b));
    }
    if let Some(a) = ao {
        stages.push((2u8, a));
    }
    if let Some(t) = to {
        stages.push((3u8, t));
    }
    Resolved {
        path,
        entry: None,
        conflicted: true,
        worktree: Some(bytes),
        mode,
        stages,
    }
}
