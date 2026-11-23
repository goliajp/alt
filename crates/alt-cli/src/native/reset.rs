//! `alt reset`: the current branch moved to another commit, with the index
//! and the files brought along as far as the mode says, or entries of the
//! index put back at the paths given.

use std::collections::{BTreeSet, HashMap};

use super::tree_edit::TreeEdit;
use super::*;
use pathspec::Spec;

/// How far a reset to a commit reaches.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ResetMode {
    /// the branch alone
    Soft,
    /// the branch and the index
    Mixed,
    /// the branch, the index and the files
    Hard,
}

impl ResetMode {
    fn name(self) -> &'static str {
        match self {
            ResetMode::Soft => "soft",
            ResetMode::Mixed => "mixed",
            ResetMode::Hard => "hard",
        }
    }
}

/// How a reset says what it did.
#[derive(Clone, Copy)]
pub struct Said {
    pub quiet: bool,
    pub json: bool,
}

pub(super) fn not_a_revision(rev: &str) -> Box<dyn std::error::Error> {
    let said = format!(
        "fatal: ambiguous argument '{rev}': unknown revision or path not in the working tree.\n\
         Use '--' to separate paths from revisions, like this:\n\
         'git <command> [<revision>...] -- [<file>...]'\n"
    );
    said::refused(&said, 128)
}

impl NativeRepo<'_> {
    /// `alt reset [--soft | --mixed | --hard] [<commit>]`, as git has it.
    pub fn reset(
        &mut self,
        rev: Option<&str>,
        mode: ResetMode,
        said: Said,
        out: &mut impl Write,
    ) -> Res<()> {
        let branch = self.head_branch()?;
        let head = self.store.refs.resolve(&branch)?;
        let target = match rev {
            Some(rev) => Some(self.commit_named(rev)?.ok_or_else(|| not_a_revision(rev))?),
            None => head,
        };
        self.ensure_resettable(mode)?;
        let moved = match (head, target) {
            (Some(old), Some(new)) if old != new => {
                self.ensure_topic_branch_or_unborn("reset")?;
                vec![RefChange {
                    name: branch,
                    old: Some(RefTarget::Oid(old)),
                    new: Some(RefTarget::Oid(new)),
                }]
            }
            _ => Vec::new(),
        };
        if mode == ResetMode::Soft {
            if !moved.is_empty() {
                self.commit_refs_only("reset", &moved)?;
            }
            return self.say_reset(target, mode, said, out);
        }
        let entries = match target {
            Some(commit) => self.commit_entries(commit)?,
            None => Vec::new(),
        };
        let index = self.index()?;
        let mut edit = TreeEdit {
            index: tree_edit::fresh(&entries),
            files: Vec::new(),
        };
        if mode == ResetMode::Hard {
            let wanted: HashMap<&BString, &WorkEntry> =
                entries.iter().map(|entry| (&entry.path, entry)).collect();
            for path in self.paths_off(&index, &entries)? {
                edit.write(&path, wanted.get(&path).copied());
            }
        }
        // a merge or a pick that stopped here is given up with it
        self.finish_merge()?;
        self.clear_cherry_pick()?;
        self.apply_edit("reset", &moved, edit)?;
        self.settle_state()?;
        self.say_reset(target, mode, said, out)
    }

    /// Refuses a reset where an operation is under way that it cannot
    /// give up, or that the mode cannot be used in.
    fn ensure_resettable(&self, mode: ResetMode) -> Res<()> {
        use in_progress::OperationKind;
        match self.operation_kind()? {
            Some(OperationKind::Merge) if mode == ResetMode::Soft => Err(settings::fatal(
                "Cannot do a soft reset in the middle of a merge.".into(),
            )),
            None | Some(OperationKind::Merge | OperationKind::CherryPick) => Ok(()),
            Some(_) => self.ensure_no_merge_in_progress("reset"),
        }
    }

    /// The paths whose file a hard reset to `entries` has to write or
    /// remove: those `index` has otherwise, and those whose file is not as
    /// the index has it.
    pub(super) fn paths_off(&self, index: &Index, entries: &[WorkEntry]) -> Res<BTreeSet<BString>> {
        let mut off = BTreeSet::new();
        let staged = index_entries(index);
        for change in alt_worktree::changes(&staged, entries) {
            off.insert(change.path.clone());
        }
        let conflicted = index.entries.iter().filter(|e| e.stage() > 0);
        off.extend(conflicted.map(|e| e.path.clone()));
        let state = self.tree_state(alt_worktree::Untracked::No)?;
        // what a submodule has checked out is not this command's to change
        let changed = state.delta.changed.iter().filter(|w| w.mode != 0o160000);
        off.extend(changed.map(|w| w.path.clone()));
        off.extend(state.delta.deleted.iter().cloned());
        off.extend(state.delta.unmerged.iter().cloned());
        Ok(off)
    }

    /// `alt reset [<commit>] [--] <paths>`: the entries of the index at
    /// the paths as the commit has them, HEAD where none is named. The
    /// files stay as they are.
    pub fn reset_paths(
        &mut self,
        rev: Option<&str>,
        paths: &[String],
        said: Said,
        out: &mut impl Write,
    ) -> Res<()> {
        let source = match rev {
            Some(rev) => {
                let commit = self.commit_named(rev)?.ok_or_else(|| {
                    settings::fatal(format!("Failed to resolve '{rev}' as a valid tree."))
                })?;
                self.commit_entries(commit)?
            }
            None => self.head_entries()?,
        };
        let parse = |given: &String| Spec::parse(&self.root, &self.prefix, given);
        let specs: Vec<Spec> = paths.iter().map(parse).collect::<Res<_>>()?;
        let given = pathspec::Named::of(&specs);
        let index = self.index()?;
        let known: BTreeSet<&BString> = index
            .entries
            .iter()
            .map(|e| &e.path)
            .chain(source.iter().map(|e| &e.path))
            .collect();
        let mut edit = TreeEdit::of(index.clone());
        let wanted: HashMap<&BString, &WorkEntry> =
            source.iter().map(|entry| (&entry.path, entry)).collect();
        for path in known.into_iter().filter(|path| given.has(path)) {
            edit.stage(path, wanted.get(path).copied());
        }
        self.apply_edit("reset", &[], edit)?;
        self.say_reset(None, ResetMode::Mixed, said, out)
    }

    /// What git says after a reset: where HEAD is after a hard one, and
    /// after a mixed one the files that are not as the index has them.
    fn say_reset(
        &self,
        target: Option<ObjectId>,
        mode: ResetMode,
        said: Said,
        out: &mut impl Write,
    ) -> Res<()> {
        let mut unstaged: Vec<(char, BString)> = Vec::new();
        if mode == ResetMode::Mixed && (said.json || !said.quiet) {
            let state = self.tree_state(alt_worktree::Untracked::No)?;
            let changed = state.delta.changed.iter().map(|w| ('M', w.path.clone()));
            let deleted = state.delta.deleted.iter().map(|path| ('D', path.clone()));
            unstaged = changed.chain(deleted).collect();
            unstaged.sort_by(|a, b| a.1.cmp(&b.1));
        }
        if said.json {
            use crate::json::Json;
            let commit = target.map_or(Json::Null, |commit| Json::str(commit.to_string()));
            let mut fields = vec![("mode", Json::str(mode.name())), ("commit", commit)];
            let paths = unstaged.iter().map(|(_, path)| path.as_slice());
            fields.extend(crate::json::paths("unstaged", "unstaged_bytes", paths));
            return Ok(crate::json::emit(out, fields)?);
        }
        if said.quiet {
            return Ok(());
        }
        if !unstaged.is_empty() {
            writeln!(out, "Unstaged changes after reset:")?;
        }
        for (letter, path) in unstaged {
            writeln!(out, "{letter}\t{path}")?;
        }
        if let (ResetMode::Hard, Some(commit)) = (mode, target) {
            let short = self.store.odb.abbrev()?.of(&commit)?;
            writeln!(out, "HEAD is now at {short} {}", self.subject_of(commit)?)?;
        }
        Ok(())
    }

    /// The first line of the message of `commit`.
    pub(super) fn subject_of(&self, commit: ObjectId) -> Res<String> {
        let object = self.store.odb.get(&commit)?;
        let object = object.ok_or("commit missing from store")?;
        let commit = alt_git_codec::Commit::parse(&object.data)?;
        let message = String::from_utf8_lossy(commit.message());
        Ok(message.lines().next().unwrap_or_default().to_owned())
    }
}
