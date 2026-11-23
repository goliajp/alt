//! `alt restore` and the path form of `alt checkout`: files of the working
//! tree, entries of the index or both put back as the index or a commit
//! has them, at the paths given.

use std::collections::{BTreeMap, BTreeSet};

use super::tree_edit::TreeEdit;
use super::*;
use pathspec::Spec;

/// What a restore puts back, and from where.
#[derive(Default)]
pub struct Restore<'a> {
    /// the entries of the index are put back
    pub staged: bool,
    /// the files are put back
    pub worktree: bool,
    /// the commit they are taken from: the index for the files alone,
    /// else HEAD, where none is given
    pub source: Option<&'a str>,
    /// for a path in conflict, the stage its file is taken from: 2 for
    /// ours, 3 for theirs
    pub side: Option<u8>,
    /// a path the source does not have is left as it is, as `checkout`
    /// leaves it, where `restore` removes it
    pub overlay: bool,
}

fn unknown(given: &str) -> Box<dyn std::error::Error> {
    let said = format!("error: pathspec '{given}' did not match any file(s) known to git\n");
    said::refused(&said, 1)
}

impl NativeRepo<'_> {
    /// `alt restore [--staged] [--worktree] [--source <rev>] <paths>`, as
    /// git has it. Returns the paths it changed something at.
    pub fn restore(&mut self, paths: &[String], how: &Restore<'_>) -> Res<Vec<BString>> {
        let source = match (how.source, how.staged) {
            (Some(rev), _) => {
                let commit = self
                    .commit_named(rev)?
                    .ok_or_else(|| settings::fatal(format!("could not resolve '{rev}'")))?;
                Some(self.commit_entries(commit)?)
            }
            (None, true) => Some(self.head_entries()?),
            (None, false) => None,
        };
        let parse = |given: &String| Spec::parse(&self.root, &self.prefix, given);
        let specs: Vec<Spec> = paths.iter().map(parse).collect::<Res<_>>()?;
        let index = self.index()?;
        let from: BTreeMap<&BString, &WorkEntry> = source
            .iter()
            .flatten()
            .map(|entry| (&entry.path, entry))
            .collect();
        let known: BTreeSet<&BString> = index
            .entries
            .iter()
            .map(|e| &e.path)
            .chain(from.keys().copied())
            .collect();
        for spec in specs.iter().filter(|spec| !spec.exclude) {
            if !known.iter().any(|path| spec.matches(path)) {
                return Err(unknown(&spec.given));
            }
        }
        let given = pathspec::Named::of(&specs);
        let named: Vec<&BString> = known.into_iter().filter(|p| given.has(p)).collect();
        let mut edit = TreeEdit::of(index.clone());
        for path in named {
            match &source {
                None => edit.write(path, Some(&self.staged_side(&index, path, how.side)?)),
                Some(_) => {
                    let entry = from.get(path).copied();
                    if entry.is_none() && how.overlay {
                        continue;
                    }
                    if how.staged {
                        edit.stage(path, entry);
                    }
                    if how.worktree {
                        edit.write(path, entry);
                    }
                }
            }
        }
        self.apply_edit("restore", &[], edit)
    }

    /// The entry of `index` the file at `path` is restored from: the one
    /// it has there, or of a path in conflict the stage `side`.
    fn staged_side(&self, index: &Index, path: &BString, side: Option<u8>) -> Res<WorkEntry> {
        let stages = index.entries.iter().filter(|e| e.path == *path);
        let stages: Vec<&IndexEntry> = stages.collect();
        let wanted = match (stages.iter().any(|e| e.stage() > 0), side) {
            (false, _) => 0,
            (true, Some(side)) => side,
            (true, None) => {
                return Err(said::refused(
                    &format!("error: path '{path}' is unmerged\n"),
                    1,
                ));
            }
        };
        let found = stages.iter().find(|e| e.stage() == wanted);
        let entry = found.ok_or_else(|| {
            let whose = if wanted == 2 { "our" } else { "their" };
            said::refused(
                &format!("error: path '{path}' does not have {whose} version\n"),
                1,
            )
        })?;
        Ok(WorkEntry {
            path: path.clone(),
            oid: entry.oid,
            mode: entry.mode,
        })
    }
}
