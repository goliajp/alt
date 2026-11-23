//! `alt stash push`: the changes put away, and the tree as HEAD has it.

use std::collections::BTreeMap;

use super::super::tree_edit::{self, TreeEdit};
use super::super::*;
use super::{ENTRIES, Stashing, TOP};

impl NativeRepo<'_> {
    /// `alt stash [push] [-m <message>] [-u]`, as git has it.
    pub fn stash_push(
        &mut self,
        message: Option<&str>,
        untracked: bool,
        said: Stashing,
        out: &mut impl Write,
    ) -> Res<()> {
        let branch = self.head_branch()?;
        let head = self
            .store
            .refs
            .resolve(&branch)?
            .ok_or_else(|| said::refused("You do not have the initial commit yet\n", 1))?;
        let index = self.index()?;
        self.ensure_stashable(&index)?;
        if untracked {
            self.ensure_exclude_files()?;
        }
        let mode = match untracked {
            true => alt_worktree::Untracked::All,
            false => alt_worktree::Untracked::No,
        };
        let state = self.tree_state(mode)?;
        // what a submodule has checked out is recorded as the commit it is at
        let changed: Vec<WorkEntry> = state.delta.changed.clone();
        let deleted: Vec<BString> = state.delta.deleted.clone();
        let is_file = |path: &&BString| !path.ends_with(b"/");
        let loose: Vec<BString> = state
            .delta
            .untracked
            .iter()
            .filter(is_file)
            .cloned()
            .collect();
        let nothing = state.staged.is_empty() && changed.is_empty() && deleted.is_empty();
        drop(state);
        if nothing && loose.is_empty() {
            if said.json {
                use crate::json::Json;
                crate::json::emit(out, vec![("saved", Json::Null)])?;
            } else if !said.quiet {
                writeln!(out, "No local changes to save")?;
            }
            return Ok(());
        }
        let short = branch.strip_prefix("refs/heads/").unwrap_or(&branch);
        let at = self.store.odb.abbrev()?.of(&head)?;
        let on = format!("{short}: {at} {}", self.subject_of(head)?);
        let subject = match message {
            Some(message) => format!("On {short}: {message}"),
            None => format!("WIP on {on}"),
        };

        let staged = index_entries(&index);
        let index_tree = self.store_tree(&staged)?;
        let index_commit = self.stash_commit(index_tree, &[head], &format!("index on {on}\n"))?;
        let mut parents = vec![head, index_commit];
        let mut work: BTreeMap<BString, WorkEntry> = staged
            .into_iter()
            .map(|entry| (entry.path.clone(), entry))
            .collect();
        let (lfs, attrs) = (self.lfs_rules()?, self.converter()?);
        for file in changed {
            let entry = match file.mode {
                0o160000 => file,
                mode => {
                    let stored = self.stored(&file.path, mode, lfs.as_ref(), &attrs, &index)?;
                    WorkEntry {
                        path: file.path,
                        oid: stored.oid,
                        mode,
                    }
                }
            };
            work.insert(entry.path.clone(), entry);
        }
        for path in &deleted {
            work.remove(path);
        }
        if !loose.is_empty() {
            let mut entries = Vec::new();
            for path in &loose {
                let meta = std::fs::symlink_metadata(self.abs(path)?)?;
                let mode = alt_worktree::FileStat::of(&meta).git_mode();
                let stored = self.stored(path, mode, lfs.as_ref(), &attrs, &index)?;
                entries.push(WorkEntry {
                    path: path.clone(),
                    oid: stored.oid,
                    mode,
                });
            }
            let tree = self.store_tree(&entries)?;
            let message = format!("untracked files on {on}\n");
            parents.push(self.stash_commit(tree, &[], &message)?);
        }
        let work: Vec<WorkEntry> = work.into_values().collect();
        let work_tree = self.store_tree(&work)?;
        let commit = self.stash_commit(work_tree, &parents, &subject)?;
        self.store.odb.flush()?;

        let number = self
            .stash_entries()
            .first()
            .map_or(0, |latest| latest.number + 1);
        let refs = [
            RefChange {
                name: format!("{ENTRIES}{number}"),
                old: None,
                new: Some(RefTarget::Oid(commit)),
            },
            RefChange {
                name: TOP.to_owned(),
                old: self.store.refs.get(TOP).cloned(),
                new: Some(RefTarget::Oid(commit)),
            },
        ];
        // the tree as HEAD has it, and the untracked files gone
        let target = self.commit_entries(head)?;
        let mut edit = TreeEdit {
            index: tree_edit::fresh(&target),
            files: Vec::new(),
        };
        let wanted: BTreeMap<&BString, &WorkEntry> =
            target.iter().map(|entry| (&entry.path, entry)).collect();
        for path in self.paths_off(&index, &target)? {
            edit.write(&path, wanted.get(&path).copied());
        }
        for path in &loose {
            edit.write(path, None);
        }
        self.apply_edit("stash", &refs, edit)?;
        if said.json {
            use crate::json::Json;
            let fields = vec![
                ("saved", Json::str(commit.to_string())),
                ("message", Json::str(&subject)),
            ];
            crate::json::emit(out, fields)?;
        } else if !said.quiet {
            writeln!(out, "Saved working directory and index state {subject}")?;
        }
        Ok(())
    }

    /// Stores a commit of a stash entry, by whoever runs the command. The
    /// message of the entry itself ends without a newline, as git writes it.
    fn stash_commit(
        &mut self,
        tree: ObjectId,
        parents: &[ObjectId],
        message: &str,
    ) -> Res<ObjectId> {
        let (author, committer) = (self.author()?, self.committer()?);
        let algo = self.store.algo()?;
        Ok(write_commit(
            &mut self.store.odb,
            tree,
            parents,
            &author.sig(),
            &committer.sig(),
            message,
            algo,
        )?)
    }
}
