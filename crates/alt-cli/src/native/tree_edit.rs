//! A change to the index and to files of the working tree that a command
//! works out path by path, made the way a checkout is: its intent in the
//! log first, with what was there kept, so that it is finished when cut
//! short and taken back by `alt undo`.

use std::collections::{BTreeSet, HashMap, HashSet};

use super::checkout_files::apart::{Coming, respell};
use super::checkout_recovery::{CheckoutIntent, FileChange, FileState};
use super::*;
use recovery::canonical_index;

/// What a command leaves in the index and puts into the working tree.
pub(super) struct TreeEdit {
    /// the whole index to come, every stage of every path
    pub(super) index: Vec<IndexEntry>,
    /// what is put at each path written
    pub(super) files: Vec<(BString, Put)>,
}

/// What an edit puts at a path of the working tree.
pub(super) enum Put {
    /// the file an entry is checked out as
    Entry(WorkEntry),
    /// a file as it was found elsewhere in the tree, whatever the index
    /// has for it
    Found(FileState),
    /// nothing: the file that is there is removed
    Nothing,
}

impl TreeEdit {
    /// An edit that starts from `index` and writes no file.
    pub(super) fn of(index: Index) -> Self {
        TreeEdit {
            index: index.entries,
            files: Vec::new(),
        }
    }

    /// Puts `entry` in place of every stage the index has at `path`, or
    /// nothing where there is none.
    pub(super) fn stage(&mut self, path: &BString, entry: Option<&WorkEntry>) {
        self.index.retain(|e| e.path != *path);
        self.index.extend(entry.map(|w| stage_entry(w, 0)));
    }

    /// Has the file at `path` written as `entry` is checked out, or
    /// removed where there is none.
    pub(super) fn write(&mut self, path: &BString, entry: Option<&WorkEntry>) {
        let put = entry.map_or(Put::Nothing, |entry| Put::Entry(entry.clone()));
        self.files.push((path.clone(), put));
    }
}

/// The index entries a checkout of `entries` leaves.
pub(super) fn fresh(entries: &[WorkEntry]) -> Vec<IndexEntry> {
    entries.iter().map(|entry| stage_entry(entry, 0)).collect()
}

/// The paths at which two lists of index entries differ.
fn differing(before: &[IndexEntry], after: &[IndexEntry]) -> BTreeSet<BString> {
    let key = |e: &IndexEntry| (e.path.clone(), e.stage());
    let state = |e: &IndexEntry| (e.mode, e.oid, e.flags, e.extended_flags);
    let of = |entries: &[IndexEntry]| -> HashMap<_, _> {
        entries.iter().map(|e| (key(e), state(e))).collect()
    };
    let (was, now) = (of(before), of(after));
    let mut paths: BTreeSet<BString> = BTreeSet::new();
    for (at, state) in &was {
        if now.get(at) != Some(state) {
            paths.insert(at.0.clone());
        }
    }
    for (at, state) in &now {
        if was.get(at) != Some(state) {
            paths.insert(at.0.clone());
        }
    }
    paths
}

/// The files an edit writes and removes, and the paths of the index it
/// leaves, as a checkout is told of to a file system that takes names
/// differing in case for one.
fn coming<'a>(files: &'a [(BString, Put)], index: &'a Index) -> Coming<'a> {
    let submodule = |put: &Put| match put {
        Put::Entry(entry) => entry.mode == 0o160000,
        Put::Found(state) => state.mode == 0o160000,
        Put::Nothing => false,
    };
    let written = |(_, put): &&(BString, Put)| !matches!(put, Put::Nothing);
    let removed = |(_, put): &&(BString, Put)| matches!(put, Put::Nothing);
    Coming {
        come: files
            .iter()
            .filter(written)
            .map(|(p, put)| (p, submodule(put)))
            .collect(),
        leave: files.iter().filter(removed).map(|(p, _)| p).collect(),
        held: index.entries.iter().map(|e| &e.path).collect(),
    }
}

impl NativeRepo<'_> {
    /// What making `edit` changes, as the intent of a checkout: the index
    /// to come, and for each path written the file that is there and the
    /// one to come, and the paths at which either differs from what is
    /// there. Nothing is written to the working tree or the index.
    fn edit_intent(&mut self, mut edit: TreeEdit) -> Res<(CheckoutIntent, Vec<BString>)> {
        let before = self.index()?;
        edit.index
            .sort_by(|a, b| a.path.cmp(&b.path).then(a.stage().cmp(&b.stage())));
        let index = Index {
            version: 2,
            entries: edit.index,
            extensions: Vec::new(),
        };
        index_file::ensure_sound(&index)?;
        let mut touched = differing(&before.entries, &index.entries);
        let written = edit.files.iter().map(|(path, _)| path);
        for path in written.chain(&touched) {
            self.ensure_path_allowed(path)?;
        }
        // the index to come may name blobs this command stored
        self.store.odb.flush_synced()?;
        let attrs = self.converter()?;
        let among: HashSet<&[u8]> = edit.files.iter().map(|(p, _)| p.as_slice()).collect();
        let coming = coming(&edit.files, &index);
        let respelled = self.kept_apart(&coming)?;
        let owned = |p: &[u8]| among.contains(p) || respelled.contains_key(p);
        let mut changes = Vec::new();
        let mut planned = HashMap::new();
        for (path, put) in &edit.files {
            let after = match put {
                Put::Entry(entry) => {
                    let state = self.target_state(entry, &attrs)?;
                    planned.insert(path.to_vec(), (state.mode, state.contents.clone()));
                    Some(state)
                }
                Put::Found(state) => Some(state.clone()),
                Put::Nothing => None,
            };
            let gitlink = after.as_ref().is_some_and(|state| state.mode == 0o160000);
            let before = self.observed(path, true, gitlink, &owned)?;
            if before != after {
                touched.insert(path.clone());
                changes.push(FileChange {
                    path: path.to_vec(),
                    before,
                    after,
                });
            }
        }
        respell(&mut changes, &respelled);
        self.keep_respelled(&mut changes, coming.come.iter().map(|(path, _)| *path))?;
        let algo = self.store.algo()?;
        let index_after = self.snapshot(&canonical_index(index, algo))?;
        *self.planned.borrow_mut() = Some(tree_state::Planned {
            index: index_after.clone(),
            files: planned,
        });
        let intent = CheckoutIntent {
            version: 1,
            workspace: self.workspace_identity()?,
            request_id: self.request_id.map(|id| crate::machine::hex(&id)),
            index_before: self.snapshot(&canonical_index(before, algo))?,
            index_after,
            files: changes,
        };
        Ok((intent, touched.into_iter().collect()))
    }

    /// Makes `edit` as one operation the log calls `verb`, together with
    /// the ref changes `refs` where there are any. An edit that changes
    /// nothing leaves no record but that of its ref changes. Returns the
    /// paths at which the index or a file changed.
    pub(super) fn apply_edit(
        &mut self,
        verb: &str,
        refs: &[RefChange],
        edit: TreeEdit,
    ) -> Res<Vec<BString>> {
        self.ensure_writable(verb)?;
        let (intent, touched) = self.edit_intent(edit)?;
        match (refs.is_empty(), touched.is_empty()) {
            (true, true) => {}
            (true, false) => {
                let prepared = self.record_checkout(verb, &intent)?;
                self.apply_prepared_checkout(prepared, &intent)?;
            }
            (false, true) => drop(self.commit_refs_only(verb, refs)?),
            (false, false) => {
                // whatever refuses the files refuses before the refs move
                self.ensure_directories_replaceable(&intent)?;
                self.checkout_intent = Some(intent.clone());
                let result = self.commit_refs(verb, refs);
                self.checkout_intent = None;
                self.apply_prepared_checkout(result?, &intent)?;
            }
        }
        Ok(touched)
    }

    /// The commit `rev` names, `HEAD` being that of this workspace; `None`
    /// where it names nothing.
    pub(super) fn commit_named(&self, rev: &str) -> Res<Option<ObjectId>> {
        let spec = pathspec::at_head(&self.head_ref, rev);
        let repo = self.store.git_view()?;
        let Some(named) = repo.rev_parse(&spec)? else {
            return Ok(None);
        };
        let (commit, object) = repo.peel(named)?;
        Ok((object.kind == ObjectKind::Commit).then_some(commit))
    }

    /// Whether `rev` names a commit.
    pub fn names_commit(&self, rev: &str) -> Res<bool> {
        Ok(self.commit_named(rev)?.is_some())
    }

    /// Whether a file is there by the name `word`, read as a path from
    /// where the command runs.
    pub fn file_named(&self, word: &str) -> Res<bool> {
        let spec = pathspec::Spec::parse(&self.root, &self.prefix, word)?;
        Ok(self.root.join(&spec.path).symlink_metadata().is_ok())
    }

    /// What tells the words of a command line apart here: a word that
    /// names a commit, and one that names a file.
    pub fn tells(
        &self,
    ) -> crate::rev_or_path::Tells<impl Fn(&str) -> Res<bool> + '_, impl Fn(&str) -> Res<bool> + '_>
    {
        crate::rev_or_path::Tells {
            revision: |word: &str| self.names_commit(word),
            file: |word: &str| self.file_named(word),
        }
    }
}
