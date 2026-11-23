//! `alt stash push`: the changes put away, and the tree as HEAD has it.

use std::collections::BTreeMap;

use super::super::tree_edit::{self, TreeEdit};
use super::super::*;
use super::{ENTRIES, Stashing, TOP};

/// What a stash has to put away.
struct Unsaved {
    /// the tracked files that are not as the index has them
    changed: Vec<WorkEntry>,
    /// the tracked files that are gone
    deleted: Vec<BString>,
    /// the files that are not tracked, where those are put away as well
    loose: Vec<BString>,
    /// something is staged
    staged: bool,
}

impl Unsaved {
    fn nothing(&self) -> bool {
        !self.staged && self.changed.is_empty() && self.deleted.is_empty() && self.loose.is_empty()
    }
}

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
        let unsaved = self.unsaved(untracked)?;
        if unsaved.nothing() {
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
        let commit = self.stash_commits(head, &index, &unsaved, &on, &subject)?;
        self.put_away(head, &index, commit, &unsaved.loose)?;
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

    /// What the working tree has that is not committed; the untracked
    /// files among it where `untracked`.
    fn unsaved(&self, untracked: bool) -> Res<Unsaved> {
        if untracked {
            self.ensure_exclude_files()?;
        }
        let mode = match untracked {
            true => alt_worktree::Untracked::All,
            false => alt_worktree::Untracked::No,
        };
        let state = self.tree_state(mode)?;
        // a directory with a repository of its own in it is one path
        let is_file = |path: &&BString| !path.ends_with(b"/");
        Ok(Unsaved {
            changed: state.delta.changed.clone(),
            deleted: state.delta.deleted.clone(),
            loose: state
                .delta
                .untracked
                .iter()
                .filter(is_file)
                .cloned()
                .collect(),
            staged: !state.staged.is_empty(),
        })
    }

    /// Stores the commits of an entry made on `head` and returns the one
    /// that is the entry: a commit of `index`, one of the untracked files
    /// where there are any, and the commit of the working tree, whose
    /// subject is `subject`, with those as its parents after `head`.
    fn stash_commits(
        &mut self,
        head: ObjectId,
        index: &Index,
        unsaved: &Unsaved,
        on: &str,
        subject: &str,
    ) -> Res<ObjectId> {
        let staged = index_entries(index);
        let index_tree = self.store_tree(&staged)?;
        let index_commit = self.stash_commit(index_tree, &[head], &format!("index on {on}\n"))?;
        let mut parents = vec![head, index_commit];
        let mut work: BTreeMap<BString, WorkEntry> =
            staged.into_iter().map(|e| (e.path.clone(), e)).collect();
        let (lfs, attrs) = (self.lfs_rules()?, self.converter()?);
        let stored = |repo: &mut Self, path: &BString, mode: u32| -> Res<WorkEntry> {
            let stored = repo.stored(path, mode, lfs.as_ref(), &attrs, index)?;
            Ok(WorkEntry {
                path: path.clone(),
                oid: stored.oid,
                mode,
            })
        };
        for file in &unsaved.changed {
            // what a submodule has checked out is recorded as the commit it is at
            let entry = match file.mode {
                0o160000 => file.clone(),
                mode => stored(self, &file.path, mode)?,
            };
            work.insert(entry.path.clone(), entry);
        }
        for path in &unsaved.deleted {
            work.remove(path);
        }
        if !unsaved.loose.is_empty() {
            let (mut entries, modes) = (Vec::new(), self.staged_modes()?);
            for path in &unsaved.loose {
                let meta = std::fs::symlink_metadata(self.abs(path)?)?;
                let mode = modes.of(path, alt_worktree::FileStat::of(&meta).git_mode());
                entries.push(stored(self, path, mode)?);
            }
            let tree = self.store_tree(&entries)?;
            let message = format!("untracked files on {on}\n");
            parents.push(self.stash_commit(tree, &[], &message)?);
        }
        let work: Vec<WorkEntry> = work.into_values().collect();
        let work_tree = self.store_tree(&work)?;
        let commit = self.stash_commit(work_tree, &parents, subject)?;
        self.store.odb.flush()?;
        Ok(commit)
    }

    /// Puts `commit` on the stack and brings the index and the files to
    /// what `head` has, the untracked files `loose` gone, as one operation.
    fn put_away(
        &mut self,
        head: ObjectId,
        index: &Index,
        commit: ObjectId,
        loose: &[BString],
    ) -> Res<()> {
        let latest = self.stash_entries().first().map(|entry| entry.number + 1);
        let refs = [
            RefChange {
                name: format!("{ENTRIES}{}", latest.unwrap_or(0)),
                old: None,
                new: Some(RefTarget::Oid(commit)),
            },
            RefChange {
                name: TOP.to_owned(),
                old: self.store.refs.get(TOP).cloned(),
                new: Some(RefTarget::Oid(commit)),
            },
        ];
        let target = self.commit_entries(head)?;
        let mut edit = TreeEdit {
            index: tree_edit::fresh(&target),
            files: Vec::new(),
        };
        let wanted: BTreeMap<&BString, &WorkEntry> =
            target.iter().map(|entry| (&entry.path, entry)).collect();
        for path in self.paths_off(index, &target)? {
            edit.write(&path, wanted.get(&path).copied());
        }
        for path in loose {
            edit.write(path, None);
        }
        self.apply_edit("stash", &refs, edit).map(drop)
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
