//! Which two sides a diff is of: the index and the files, a commit and
//! either of them, or two commits.

use std::collections::BTreeMap;

use super::worktree::Worktree;
use super::*;
use crate::rev_or_path::{Tells, Words};

/// What a diff is asked for.
#[derive(Default)]
pub struct DiffRequest<'a> {
    /// revisions, then paths. No revision is for the index against the
    /// files, one for that commit against the files, two or `A..B` for
    /// one commit against another, `A...B` for what B has changed since
    /// the two parted; with paths, only what changed at them
    pub words: Words<'a>,
    /// the index in place of the files
    pub cached: bool,
    pub json: bool,
    pub semantic: bool,
    /// a summary in place of the patch
    pub summary: Option<Summary>,
}

/// What a diff prints in place of the patch.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Summary {
    /// the path of each file that changed
    NameOnly,
    /// a letter for how it changed, and its path
    NameStatus,
    /// how many lines changed in each, and in all
    Stat,
}

/// The two sides of a diff.
pub(super) struct Sides {
    pub(super) old: Vec<WorkEntry>,
    pub(super) new: Vec<WorkEntry>,
    /// what the new side is read with where it is the working tree
    pub(super) disk: Option<Worktree>,
    /// a file only the new side has is part of the diff: not between the
    /// index and the files, where such a file is untracked
    pub(super) added: bool,
}

impl NativeRepo<'_> {
    fn diff_commit(&self, rev: &str) -> Res<ObjectId> {
        self.commit_named(rev)?
            .ok_or_else(|| reset::not_a_revision(rev))
    }

    /// The tracked files as the working tree has them: the entries of the
    /// index, those of the files that differ put in their place.
    fn worktree_entries(&self) -> Res<(Vec<WorkEntry>, Worktree)> {
        let (staged, changed, disk) = self.worktree_sides()?;
        let index = index_entries(&self.index()?);
        let mut entries: BTreeMap<BString, WorkEntry> =
            index.into_iter().map(|e| (e.path.clone(), e)).collect();
        // `staged` holds the paths whose file changed or is gone
        for entry in staged {
            entries.remove(&entry.path);
        }
        entries.extend(changed.into_iter().map(|e| (e.path.clone(), e)));
        Ok((entries.into_values().collect(), disk))
    }

    /// The sides the revisions among `words` and `cached` ask for, and the
    /// paths among them.
    pub(super) fn diff_sides<'a>(
        &self,
        words: &Words<'a>,
        cached: bool,
    ) -> Res<(Sides, &'a [String])> {
        let commit = |rev: &str| self.names_commit(rev);
        let tells = Tells {
            revision: |word: &str| match crate::log_range::sides(word) {
                Some((left, right, _)) => Ok(commit(left)? && commit(right)?),
                None => commit(word),
            },
            file: |word: &str| self.file_named(word),
        };
        let (revs, paths) = words.revisions_then_paths(&tells)?;
        Ok((self.sides_of(revs, cached)?, paths))
    }

    fn sides_of(&self, revs: &[String], cached: bool) -> Res<Sides> {
        let between = |old: ObjectId, new: ObjectId| -> Res<Sides> {
            Ok(Sides {
                old: self.commit_entries(old)?,
                new: self.commit_entries(new)?,
                disk: None,
                added: true,
            })
        };
        let range = match revs {
            [one] => crate::log_range::sides(one),
            _ => None,
        };
        match (revs, range, cached) {
            ([], _, true) => {
                let staged = self.staged_changes()?;
                let side = |pick: fn(alt_worktree::Staged) -> Option<WorkEntry>| {
                    staged.iter().cloned().filter_map(pick).collect()
                };
                Ok(Sides {
                    old: side(|s| s.old),
                    new: side(|s| s.new),
                    disk: None,
                    added: true,
                })
            }
            ([], _, false) => {
                let (old, new, disk) = self.worktree_sides()?;
                Ok(Sides {
                    old,
                    new,
                    disk: Some(disk),
                    added: false,
                })
            }
            (_, Some((left, right, false)), _) => {
                between(self.diff_commit(left)?, self.diff_commit(right)?)
            }
            (_, Some((left, right, true)), _) => {
                let (left, right) = (self.diff_commit(left)?, self.diff_commit(right)?);
                let bases = self.merge_bases(&[left], &[right])?;
                let base = bases
                    .first()
                    .ok_or_else(|| settings::fatal(format!("{}: no merge base", revs[0])))?;
                between(*base, right)
            }
            ([rev], None, true) => Ok(Sides {
                old: self.commit_entries(self.diff_commit(rev)?)?,
                new: index_entries(&self.index()?),
                disk: None,
                added: true,
            }),
            ([rev], None, false) => {
                let old = self.commit_entries(self.diff_commit(rev)?)?;
                let (new, disk) = self.worktree_entries()?;
                Ok(Sides {
                    old,
                    new,
                    disk: Some(disk),
                    added: true,
                })
            }
            ([old, new], ..) => between(self.diff_commit(old)?, self.diff_commit(new)?),
            ([_, _, more, ..], ..) => Err(reset::not_a_revision(more)),
        }
    }
}
