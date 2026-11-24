//! `alt stash apply` and `pop`: the changes of an entry merged into the
//! working tree.

use std::collections::{BTreeMap, HashSet};

use super::super::checkout_recovery::FileState;
use super::super::merge::Resolved;
use super::super::tree_edit::{self, Put, TreeEdit};
use super::super::*;
use super::{Entry, Stashing};

/// What a conflict calls the two sides, as git does.
const SIDES: (&str, &str) = ("Updated upstream", "Stashed changes");

fn by_path(entries: &[WorkEntry]) -> BTreeMap<&BString, &WorkEntry> {
    entries.iter().map(|entry| (&entry.path, entry)).collect()
}

fn same(a: Option<&WorkEntry>, b: Option<&WorkEntry>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => a.oid == b.oid && a.mode == b.mode,
        _ => false,
    }
}

/// The index as a merge that stopped leaves it: each path as it resolved,
/// a conflict in its stages.
fn as_merged(resolved: &[Resolved]) -> Vec<IndexEntry> {
    let mut index = Vec::new();
    for r in resolved {
        let stages = r
            .stages
            .iter()
            .map(|(stage, entry)| stage_entry(entry, *stage));
        match r.conflicted {
            true => index.extend(stages),
            false => index.extend(r.entry.iter().map(|e| stage_entry(e, 0))),
        }
    }
    index
}

impl NativeRepo<'_> {
    /// `alt stash apply [--index] [<entry>]`, and `pop`, which takes the
    /// entry out of the stack as well when nothing conflicts. Returns
    /// whether it stopped in conflict.
    pub fn stash_apply(
        &mut self,
        named: Option<&str>,
        with_index: bool,
        pop: bool,
        said: Stashing,
        out: &mut impl Write,
    ) -> Res<bool> {
        let (at, entry, called) = self.stash_entry(named)?;
        let parents = self.commit_parents(entry.commit)?;
        let [base, staged, rest @ ..] = &parents[..] else {
            return Err(settings::fatal(format!(
                "'{}' is not a stash-like commit",
                entry.commit
            )));
        };
        let index = self.index()?;
        self.ensure_stashable(&index)?;
        let ours = index_entries(&index);
        let base = self.commit_entries(*base)?;
        let theirs = self.commit_entries(entry.commit)?;
        let resolved = self.merge_trees_named(&base, &ours, &theirs, SIDES)?;
        let loose = match rest.first() {
            Some(untracked) => self.commit_entries(*untracked)?,
            None => Vec::new(),
        };
        let have = by_path(&ours);
        let changes =
            |r: &&Resolved| r.conflicted || !same(r.entry.as_ref(), have.get(&r.path).copied());
        let changed: Vec<&Resolved> = resolved.iter().filter(changes).collect();
        let blocked = self.ensure_applicable(&changed, &have, &loose)?;

        let conflicts: Vec<BString> = resolved
            .iter()
            .filter(|r| r.conflicted)
            .map(|r| r.path.clone())
            .collect();
        let mut edit = TreeEdit::of(index);
        if !conflicts.is_empty() {
            edit.index = as_merged(&resolved);
        } else if with_index {
            let wanted = self.commit_entries(*staged)?;
            edit.index = self.with_staged(&base, &ours, &wanted)?;
        } else {
            // what the entry adds is staged, as by git; the rest is left unstaged
            for r in changed.iter().filter(|r| !have.contains_key(&r.path)) {
                edit.stage(&r.path, r.entry.as_ref());
            }
        }
        self.store.odb.flush()?;
        let attrs = self.converter()?;
        for r in &changed {
            match &r.worktree {
                Some(bytes) if r.conflicted => {
                    let converted = attrs.to_worktree(&r.path, bytes)?;
                    let mode = if r.mode == 0o100755 { r.mode } else { 0o100644 };
                    let contents = self.snapshot(converted.as_deref().unwrap_or(bytes))?;
                    edit.files
                        .push((r.path.clone(), Put::Found(FileState { mode, contents })));
                }
                _ => edit.write(&r.path, r.entry.as_ref()),
            }
        }
        for file in loose.iter().filter(|file| !blocked.contains(&file.path)) {
            edit.write(&file.path, Some(file));
        }
        let kept = !conflicts.is_empty() || !blocked.is_empty();
        let refs = match pop && !kept {
            true => self.stash_removal(at),
            false => Vec::new(),
        };
        self.apply_edit("stash", &refs, edit)?;
        let popped = pop.then_some((called.as_str(), entry));
        if blocked.is_empty() {
            self.say_applied(&conflicts, popped, said, out)?;
            return Ok(!conflicts.is_empty());
        }
        self.fail_blocked(&blocked, pop, said, out)
    }

    /// Ends an apply that left the untracked files `blocked` out. As by
    /// git, the rest of the entry is applied and the entry stays.
    fn fail_blocked(
        &self,
        blocked: &[BString],
        pop: bool,
        said: Stashing,
        out: &mut impl Write,
    ) -> Res<bool> {
        if !said.quiet && !said.json {
            self.status(None, false, out)?;
            if pop {
                writeln!(out, "The stash entry is kept in case you need it again.")?;
            }
        }
        out.flush()?;
        let mut told = String::new();
        for path in blocked {
            told.push_str(&format!("{path} already exists, no checkout\n"));
        }
        told.push_str("error: could not restore untracked files from stash\n");
        Err(said::refused(&told, 128))
    }

    /// The index `ours` with what the entry had staged, `wanted`, merged
    /// into it over `base`; refused where the two cannot be merged.
    fn with_staged(
        &mut self,
        base: &[WorkEntry],
        ours: &[WorkEntry],
        wanted: &[WorkEntry],
    ) -> Res<Vec<IndexEntry>> {
        let merged = self.merge_trees_named(base, ours, wanted, SIDES)?;
        if merged.iter().any(|r| r.conflicted) {
            let said = "error: Conflicts in index. Try without --index.\n";
            return Err(said::refused(said, 1));
        }
        let entries: Vec<WorkEntry> = merged.into_iter().filter_map(|r| r.entry).collect();
        Ok(tree_edit::fresh(&entries))
    }

    /// Refuses, before anything is written, an apply that would write
    /// over a change that is not staged or over a file that is not
    /// tracked. Returns the untracked files of the entry that a file is
    /// in the way of: those are left out, and the apply fails once the
    /// rest is done.
    fn ensure_applicable(
        &self,
        changed: &[&Resolved],
        have: &BTreeMap<&BString, &WorkEntry>,
        loose: &[WorkEntry],
    ) -> Res<Vec<BString>> {
        let state = self.tree_state(alt_worktree::Untracked::No)?;
        let dirty = state.delta.changed.iter().filter(|w| w.mode != 0o160000);
        let dirty: HashSet<&BString> = dirty.map(|w| &w.path).chain(&state.delta.deleted).collect();
        let lost: Vec<BString> = changed
            .iter()
            .filter(|r| dirty.contains(&r.path))
            .map(|r| r.path.clone())
            .collect();
        if !lost.is_empty() {
            return Err(said::would_overwrite(
                "Your local changes to the following files",
                &lost,
                "merge",
                "Please commit your changes or stash them before you merge.",
            ));
        }
        let there = |path: &BString| -> Res<bool> {
            Ok(self
                .abs(path)?
                .symlink_metadata()
                .is_ok_and(|m| !m.is_dir()))
        };
        let mut in_the_way = Vec::new();
        for r in changed.iter().filter(|r| !have.contains_key(&r.path)) {
            if there(&r.path)? {
                in_the_way.push(r.path.clone());
            }
        }
        self.ensure_apart(&in_the_way, have.keys().copied())?;
        if !in_the_way.is_empty() {
            return Err(said::would_overwrite(
                "The following untracked working tree files",
                &in_the_way,
                "merge",
                "Please move or remove them before you merge.",
            ));
        }
        self.blocked_by(changed, loose)
    }

    /// The untracked files `loose` of an entry that a file is in the way
    /// of once the changes `changed` are applied.
    fn blocked_by(&self, changed: &[&Resolved], loose: &[WorkEntry]) -> Res<Vec<BString>> {
        let mut blocked = Vec::new();
        for file in loose {
            let written = changed.iter().find(|r| r.path == file.path);
            let stays = match written {
                Some(r) => r.conflicted || r.entry.is_some(),
                None => {
                    let meta = self.abs(&file.path)?.symlink_metadata();
                    meta.is_ok_and(|m| !m.is_dir())
                }
            };
            if stays {
                blocked.push(file.path.clone());
            }
        }
        Ok(blocked)
    }

    /// What an apply says: the conflicts where there are any, else the
    /// state of the tree as `alt status` shows it, and the entry a pop
    /// took out of the stack.
    fn say_applied(
        &self,
        conflicts: &[BString],
        popped: Option<(&str, Entry)>,
        said: Stashing,
        out: &mut impl Write,
    ) -> Res<()> {
        let dropped = popped.filter(|_| conflicts.is_empty());
        if said.json {
            use crate::json::Json;
            let commit = |(_, entry): (&str, Entry)| Json::str(entry.commit.to_string());
            let mut fields = vec![("dropped", dropped.map_or(Json::Null, commit))];
            let paths = conflicts.iter().map(|path| path.as_slice());
            fields.extend(crate::json::paths("conflicts", "conflicts_bytes", paths));
            return Ok(crate::json::emit(out, fields)?);
        }
        if !conflicts.is_empty() {
            cherry_pick::say_conflicts(out, "Merge", conflicts)?;
            if popped.is_some() {
                writeln!(out, "The stash entry is kept in case you need it again.")?;
            }
            return Ok(());
        }
        if !said.quiet {
            self.status(None, false, out)?;
        }
        match dropped {
            Some((called, entry)) => self.say_dropped(called, entry, said, out),
            None => Ok(()),
        }
    }
}
