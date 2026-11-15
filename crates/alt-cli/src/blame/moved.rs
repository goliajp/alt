//! Lines a commit moved within the blamed file or copied from another
//! file (`-M` and `-C`): what its parents' diffs left with the commit is
//! compared once more against the parents' files, and a run of lines found
//! there, if it carries enough text, is traced on through that file.

use alt_bytes::BString;

use super::Res;
use super::walk::{Board, CommitInfo, Entry};
use crate::tree_changes::{self, TreeFile};

/// An entry cut in three: the middle is found in a parent's file, at the
/// line `found`; what is in front of it and behind it is not.
#[derive(Clone, Copy)]
struct Split {
    pre: Option<Entry>,
    found: Entry,
    post: Option<Entry>,
}

impl Board<'_> {
    /// Looks for moved and copied lines among what is still pending at
    /// origin `at`, whose commit is `info`; `takers` holds, per parent,
    /// the origin of the blamed file there.
    pub(super) fn pass_moved(
        &mut self,
        at: usize,
        info: &CommitInfo,
        takers: &[Option<usize>],
    ) -> Res<()> {
        let options = self.options;
        if !options.moves && options.copies == 0 {
            return Ok(());
        }
        let mut small = Vec::new();
        let mut pending = std::mem::take(&mut self.origins[at].pending);
        self.set_aside(&mut pending, &mut small, options.move_score);
        for &parent in takers.iter().flatten() {
            if pending.is_empty() {
                break;
            }
            let blob = self.blob(self.origins[parent].file.oid)?;
            pending = self.search(pending, &mut small, options.move_score, &[(parent, blob)])?;
        }
        if options.copies > 0 {
            // what was too little to count as moved may be enough to
            // count as copied
            if options.copy_score < options.move_score {
                pending.append(&mut small);
                pending.sort_by_key(|e| e.line);
            }
            self.set_aside(&mut pending, &mut small, options.copy_score);
            for (i, &parent) in info.parents.iter().enumerate() {
                if pending.is_empty() {
                    break;
                }
                let files = self.copy_sources(at, info, parent, takers[i])?;
                pending = self.search(pending, &mut small, options.copy_score, &files)?;
            }
        }
        pending.extend(small);
        self.origins[at].pending = pending;
        Ok(())
    }

    /// Moves the entries that carry too little text to be searched for
    /// from `entries` to `small`.
    fn set_aside(&self, entries: &mut Vec<Entry>, small: &mut Vec<Entry>, least: usize) {
        let (keep, drop): (Vec<Entry>, Vec<Entry>) =
            entries.iter().partition(|e| self.score(e) > least);
        small.extend(drop);
        *entries = keep;
    }

    /// How much text an entry carries: its letters and digits, plus one.
    fn score(&self, e: &Entry) -> usize {
        let text = &self.content[self.starts[e.line]..self.starts[e.line + e.len]];
        1 + text.iter().filter(|b| b.is_ascii_alphanumeric()).count()
    }

    /// Searches `files`, each an origin with its content, for the lines of
    /// `entries`. For each entry the best run found in any file is handed
    /// to that origin when it scores above `least`; the lines around it
    /// are searched again. Returns the entries nothing was found for.
    fn search(
        &mut self,
        mut entries: Vec<Entry>,
        small: &mut Vec<Entry>,
        least: usize,
        files: &[(usize, std::rc::Rc<Vec<u8>>)],
    ) -> Res<Vec<Entry>> {
        let mut left = Vec::new();
        while !entries.is_empty() {
            let mut again = Vec::new();
            for e in entries {
                let mut best: Option<(usize, Split)> = None;
                for (origin, blob) in files {
                    // of equally good runs the last one found is taken
                    if let Some(split) = self.best_run(&e, blob)
                        && best
                            .is_none_or(|(_, b)| self.score(&split.found) >= self.score(&b.found))
                    {
                        best = Some((*origin, split));
                    }
                }
                match best.filter(|(_, split)| self.score(&split.found) > least) {
                    Some((origin, split)) => {
                        again.extend(split.pre);
                        again.extend(split.post);
                        self.receive(origin, vec![split.found])?;
                    }
                    None => left.push(e),
                }
            }
            self.set_aside(&mut again, small, least);
            entries = again;
        }
        Ok(left)
    }

    /// The run of lines of `e` that `blob` holds and that carries the most
    /// text, with where in `blob` it is.
    fn best_run(&self, e: &Entry, blob: &[u8]) -> Option<Split> {
        let text = &self.content[self.starts[e.line]..self.starts[e.line + e.len]];
        let edits = alt_diff::myers::diff_without_context(blob, text);
        let mut best: Option<Split> = None;
        // the unchanged runs lie between the edits: (first line of the
        // run in the entry, the line in the blob, the end of the run)
        let ends = edits.iter().map(|edit| (edit.old.end, edit.new.end));
        let starts = std::iter::once((0, 0)).chain(ends);
        let stops = edits.iter().map(|edit| edit.new.start).chain([e.len]);
        for ((blob_line, from), to) in starts.zip(stops) {
            if e.len <= from || to <= from {
                continue;
            }
            let found = Entry {
                line: e.line + from,
                at: blob_line,
                len: to - from,
            };
            if best.is_some_and(|b| self.score(&found) < self.score(&b.found)) {
                continue;
            }
            let part = |from: usize, len: usize| {
                (len > 0).then_some(Entry {
                    line: e.line + from,
                    at: e.at + from,
                    len,
                })
            };
            best = Some(Split {
                pre: part(0, from),
                found,
                post: part(to, e.len - to),
            });
        }
        best
    }

    /// The files of `parent` that lines may have been copied from, as
    /// origins with their content: the files the commit changed or
    /// removed, or every file of the parent when looking harder. The
    /// blamed file itself, `own`, was searched for moves already.
    fn copy_sources(
        &mut self,
        at: usize,
        info: &CommitInfo,
        parent: alt_git_codec::ObjectId,
        own: Option<usize>,
    ) -> Res<Vec<(usize, std::rc::Rc<Vec<u8>>)>> {
        let own_path = own.map(|o| self.origins[o].path.clone());
        let created = own_path.as_ref() != Some(&self.origins[at].path);
        let harder = self.options.copies >= 3 || (self.options.copies == 2 && created);
        let parent_tree = self.commit(parent)?.tree;
        let candidates: Vec<(BString, TreeFile)> = if harder {
            let all = tree_changes::changes(self.repo, None, Some(parent_tree))?;
            all.into_iter()
                .filter_map(|c| Some((c.path, c.new?)))
                .collect()
        } else {
            let changes = tree_changes::changes(self.repo, Some(parent_tree), Some(info.tree))?;
            tree_changes::pair_renames(self.repo, changes)?
                .into_iter()
                .filter_map(|d| Some((d.old_path.unwrap_or(d.path), d.old?)))
                .collect()
        };
        let mut files = Vec::new();
        for (path, file) in candidates {
            if file.mode == 0o160000 || Some(&path) == own_path.as_ref() {
                continue;
            }
            let blob = self.blob(file.oid)?;
            files.push((self.origin(parent, path, file), blob));
        }
        Ok(files)
    }
}
