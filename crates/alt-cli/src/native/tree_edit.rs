//! A change to the index and to files of the working tree that a command
//! works out path by path, made the way a checkout is: its intent in the
//! log first, with what was there kept, so that it is finished when cut
//! short and taken back by `alt undo`.

use std::collections::{BTreeSet, HashMap, HashSet};

use super::checkout_recovery::{CheckoutIntent, FileChange};
use super::*;
use recovery::canonical_index;

/// What a command leaves in the index and puts into the working tree.
pub(super) struct TreeEdit {
    /// the whole index to come, every stage of every path
    pub(super) index: Vec<IndexEntry>,
    /// the paths written: with the file of an entry, or with none
    pub(super) files: Vec<(BString, Option<WorkEntry>)>,
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
        self.files.push((path.clone(), entry.cloned()));
    }
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
        let mut changes = Vec::new();
        let mut planned = HashMap::new();
        for (path, entry) in &edit.files {
            let after = match entry {
                Some(entry) => {
                    let state = self.target_state(entry, &attrs)?;
                    planned.insert(path.to_vec(), (state.mode, state.contents.clone()));
                    Some(state)
                }
                None => None,
            };
            let gitlink = entry.as_ref().is_some_and(|e| e.mode == 0o160000);
            let before = self.observed(path, true, gitlink, &|p| among.contains(p))?;
            if before != after {
                touched.insert(path.clone());
                changes.push(FileChange {
                    path: path.to_vec(),
                    before,
                    after,
                });
            }
        }
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
        let spec = match rev.strip_prefix("HEAD").or_else(|| rev.strip_prefix('@')) {
            Some(rest) if rest.is_empty() || rest.starts_with(['~', '^']) => {
                format!("{}{rest}", self.head_ref)
            }
            _ => rev.to_owned(),
        };
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
}
