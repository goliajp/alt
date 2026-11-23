//! `alt rm`: files taken out of the index, and out of the working tree
//! unless only the index is asked for.

use std::collections::{BTreeSet, HashSet};

use super::tree_edit::TreeEdit;
use super::*;
use pathspec::Spec;

/// How an `rm` goes about it.
#[derive(Clone, Copy, Default)]
pub struct Rm {
    /// the files stay, only the index entries go
    pub cached: bool,
    /// a path given may name a directory
    pub recursive: bool,
    /// a file with changes that are nowhere else is removed all the same
    pub force: bool,
    /// a path that names no file of the index is passed over
    pub ignore_unmatch: bool,
    /// nothing is removed: the paths are only said
    pub dry_run: bool,
}

/// git's refusal to remove `paths`, for one of its reasons.
fn in_the_way(said: &mut String, paths: &[&BString], what: (&str, &str), hint: &str) {
    if paths.is_empty() {
        return;
    }
    let (one, several) = what;
    let what = if paths.len() == 1 { one } else { several };
    said.push_str(&format!("error: {what}\n"));
    for path in paths {
        said.push_str(&format!("    {path}\n"));
    }
    said.push_str(&format!("{hint}\n"));
}

impl NativeRepo<'_> {
    /// `alt rm <paths>`, as git has it. Returns the paths removed, in
    /// order.
    pub fn rm(&mut self, paths: &[String], how: Rm) -> Res<Vec<BString>> {
        let parse = |given: &String| Spec::parse(&self.root, &self.prefix, given);
        let specs: Vec<Spec> = paths.iter().map(parse).collect::<Res<_>>()?;
        let index = self.index()?;
        let tracked: BTreeSet<&BString> = index.entries.iter().map(|e| &e.path).collect();
        for spec in specs.iter().filter(|spec| !spec.exclude) {
            let mut named = tracked.iter().filter(|path| spec.matches(path)).peekable();
            if named.peek().is_none() && !how.ignore_unmatch {
                let given = &spec.given;
                return Err(settings::fatal(format!(
                    "pathspec '{given}' did not match any files"
                )));
            }
            // a path written out names what lies beneath it only with -r
            let beneath =
                |path: &&&BString| spec.is_plain() && path.as_slice() != spec.path.as_bytes();
            if !how.recursive && named.any(|path| beneath(&path)) {
                let given = &spec.given;
                return Err(settings::fatal(format!(
                    "not removing '{given}' recursively without -r"
                )));
            }
        }
        let given = pathspec::Named::of(&specs);
        let removed: Vec<BString> = tracked
            .into_iter()
            .filter(|p| given.has(p))
            .cloned()
            .collect();
        if !how.force {
            self.ensure_removable(&index, &removed, how.cached)?;
        }
        if how.dry_run {
            return Ok(removed);
        }
        let mut edit = TreeEdit::of(index);
        for path in &removed {
            edit.stage(path, None);
            if !how.cached {
                edit.write(path, None);
            }
        }
        self.apply_edit("rm", &[], edit)?;
        Ok(removed)
    }

    /// Refuses, as git does, to remove a file whose content would be
    /// nowhere after: one staged otherwise than HEAD has it, or changed
    /// since it was staged. A file that is gone has nothing to lose, and
    /// a path in conflict is removed with its stages.
    fn ensure_removable(&self, index: &Index, removed: &[BString], cached: bool) -> Res<()> {
        let state = self.tree_state(alt_worktree::Untracked::No)?;
        let changed: HashSet<&BString> = state.delta.changed.iter().map(|w| &w.path).collect();
        let gone: HashSet<&BString> = state.delta.deleted.iter().collect();
        let staged: HashSet<&BString> = state.staged.iter().map(|s| &s.path).collect();
        let conflicted = index.entries.iter().filter(|e| e.stage() > 0);
        let conflicted: HashSet<&BString> = conflicted.map(|e| &e.path).collect();
        let (mut both, mut in_index, mut local) = (Vec::new(), Vec::new(), Vec::new());
        for path in removed {
            if gone.contains(path) || conflicted.contains(path) {
                continue;
            }
            match (staged.contains(path), changed.contains(path)) {
                (true, true) => both.push(path),
                (true, false) if !cached => in_index.push(path),
                (false, true) if !cached => local.push(path),
                _ => {}
            }
        }
        let mut said = String::new();
        let keep = "(use --cached to keep the file, or -f to force removal)";
        in_the_way(
            &mut said,
            &both,
            (
                "the following file has staged content different from both the\nfile and the HEAD:",
                "the following files have staged content different from both the\nfile and the HEAD:",
            ),
            "(use -f to force removal)",
        );
        in_the_way(
            &mut said,
            &in_index,
            (
                "the following file has changes staged in the index:",
                "the following files have changes staged in the index:",
            ),
            keep,
        );
        in_the_way(
            &mut said,
            &local,
            (
                "the following file has local modifications:",
                "the following files have local modifications:",
            ),
            keep,
        );
        match said.is_empty() {
            true => Ok(()),
            false => Err(said::refused(&said, 1)),
        }
    }
}
