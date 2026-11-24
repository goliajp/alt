//! `alt switch`: HEAD at another branch, with the files of its commit.
//! What is not committed goes along where the two commits agree on the
//! path, as it does for git.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use super::*;

mod made;

/// The entry at each path of `entries`.
fn by_path(entries: &[WorkEntry]) -> BTreeMap<&BString, &WorkEntry> {
    entries.iter().map(|e| (&e.path, e)).collect()
}

fn same(a: Option<&WorkEntry>, b: Option<&WorkEntry>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => a.oid == b.oid && a.mode == b.mode,
        _ => false,
    }
}

/// The index after HEAD goes from the tree `head` to the tree `target`
/// with what is staged in `index` kept, as git's two-way merge has it: a
/// path the two trees agree on stays as it is staged, and one they
/// differ on takes the target's entry where nothing is staged there and
/// its file is not among the `dirty`. The paths where a change would be
/// lost, if there are any, are the error.
fn carried(
    head: &[WorkEntry],
    target: &[WorkEntry],
    index: &[WorkEntry],
    dirty: &HashSet<&BString>,
) -> Result<Vec<WorkEntry>, Vec<BString>> {
    let (head, target, index) = (by_path(head), by_path(target), by_path(index));
    let paths: BTreeSet<&BString> = head
        .keys()
        .chain(target.keys())
        .chain(index.keys())
        .copied()
        .collect();
    let (mut kept, mut lost) = (Vec::new(), Vec::new());
    for path in paths {
        let (h, t, i) = (
            head.get(path).copied(),
            target.get(path).copied(),
            index.get(path).copied(),
        );
        if same(h, t) || same(i, t) {
            kept.extend(i.cloned());
        } else if same(i, h) && !dirty.contains(path) {
            kept.extend(t.cloned());
        } else {
            lost.push(path.clone());
        }
    }
    match lost.is_empty() {
        true => Ok(kept),
        false => Err(lost),
    }
}

/// git's list of what differs from the commit switched to: a letter and
/// the path, for each path of `index` or with a file among `dirty` or
/// `gone` that is not as the tree `target` has it.
fn local_changes(
    target: &[WorkEntry],
    index: &[WorkEntry],
    dirty: &HashSet<&BString>,
    gone: &HashSet<&BString>,
) -> Vec<(char, BString)> {
    let (target, index) = (by_path(target), by_path(index));
    let paths: BTreeSet<&BString> = target.keys().chain(index.keys()).copied().collect();
    let mut changes = Vec::new();
    for path in paths {
        let (t, i) = (target.get(path).copied(), index.get(path).copied());
        let letter = match (t, i) {
            (None, _) => 'A',
            (Some(_), None) => 'D',
            (Some(_), Some(_)) if gone.contains(path) => 'D',
            (Some(_), Some(_)) if !same(t, i) || dirty.contains(path) => 'M',
            _ => continue,
        };
        changes.push((letter, path.clone()));
    }
    changes
}

/// How a switch makes the branch it goes to.
#[derive(Clone, Copy)]
pub struct Create<'a> {
    /// where the branch starts, the current commit where nothing is named
    pub start: Option<&'a str>,
    /// a branch of the name is put there, as by `-C`
    pub over: bool,
}

impl NativeRepo<'_> {
    /// `alt switch` as the command of that name: refused while a
    /// cherry-pick is under way, as by git, which `checkout` is not.
    pub fn switch_command(
        &mut self,
        name: &str,
        create: Option<Create>,
        json: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        if let Some(in_progress::OperationKind::CherryPick) = self.operation_kind()? {
            let said = "fatal: cannot switch branch while cherry-picking\n\
                            Consider \"git cherry-pick --quit\" or \"git worktree add\".\n";
            return Err(said::refused(said, 128));
        }
        self.switch(name, create, json, out)
    }

    /// `alt switch <name>`: point HEAD at branch `name` and materialize its
    /// tree into the working tree. `-c` creates the branch first, at the
    /// current commit or at the one named for it to start at. Staged
    /// and unstaged changes are kept where the commit switched to has the
    /// path as the current one has it; a change it would write over
    /// refuses the switch, with the paths named.
    pub fn switch(
        &mut self,
        name: &str,
        create: Option<Create>,
        json: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        self.ensure_no_merge_in_progress("switch")?;
        let full = format!("refs/heads/{name}");
        let current = self.head_branch()?;
        let mut follows = None;
        let there = self.store.refs.get(&full).is_some();
        let (result, human) = if let Some(create) = create.filter(|c| c.over && there) {
            let changes = self.switch_reset(name, &full, &current, create.start)?;
            if !json {
                for (letter, path) in changes {
                    writeln!(out, "{letter}\t{path}")?;
                }
            }
            match full == current {
                true => ("reset", format!("Reset branch '{name}'")),
                false => ("reset", format!("Switched to and reset branch '{name}'")),
            }
        } else if let Some(create) = create {
            match create.start {
                None => self.switch_create(name, &full, &current)?,
                Some(start) => {
                    let changes = self.switch_create_at(name, &full, &current, start)?;
                    if !json {
                        for (letter, path) in changes {
                            writeln!(out, "{letter}\t{path}")?;
                        }
                    }
                    follows = self.followed(start)?;
                }
            }
            ("created", format!("Switched to a new branch '{name}'"))
        } else if self.store.refs.get(&full).is_none() {
            return Err(crate::machine::Failure::refusal(
                "precondition_failed",
                format!("invalid reference: {name}"),
            )
            .into());
        } else if full == current {
            ("already_on", format!("Already on '{name}'"))
        } else {
            let changes = self.switch_to(&full, &current, None)?;
            if !json {
                for (letter, path) in changes {
                    writeln!(out, "{letter}\t{path}")?;
                }
            }
            ("switched", format!("Switched to branch '{name}'"))
        };
        // emit either the human line or `{schema_version, branch, result}`
        if json {
            use crate::json::Json;
            crate::json::emit(
                out,
                vec![("branch", Json::str(name)), ("result", Json::str(result))],
            )?;
        } else {
            writeln!(out, "{human}")?;
        }
        if let Some(followed) = follows {
            let said = self.follow(name, &followed)?;
            if !json {
                writeln!(out, "{said}")?;
            }
        }
        Ok(())
    }

    /// Moves HEAD from the branch `current` to the branch `full`, which
    /// is made at `new` or put there where that is given, and checks
    /// out what differs between their commits; what is left that differs
    /// from the commit switched to.
    pub(super) fn switch_to(
        &mut self,
        full: &str,
        current: &str,
        new: Option<ObjectId>,
    ) -> Res<Vec<(char, BString)>> {
        let tree_of = |repo: &Self, commit: Option<ObjectId>| match commit {
            Some(commit) => repo.commit_entries(commit),
            None => Ok(Vec::new()), // unborn: the tree is empty
        };
        let head = tree_of(self, self.store.refs.resolve(current)?)?;
        let target = match new {
            Some(commit) => tree_of(self, Some(commit))?,
            None => tree_of(self, self.store.refs.resolve(full)?)?,
        };
        let index = self.index()?;
        if let Some(unmerged) = index.entries.iter().find(|e| e.stage() > 0) {
            let said = format!(
                "{}: needs merge\nerror: you need to resolve your current index first\n",
                unmerged.path
            );
            return Err(said::refused(&said, 1));
        }
        let state = self.tree_state(alt_worktree::Untracked::No)?;
        // what a submodule has checked out is nothing a checkout writes over
        let changed = state.delta.changed.iter().filter(|w| w.mode != 0o160000);
        let dirty: HashSet<&BString> = changed.map(|w| &w.path).collect();
        let gone: HashSet<&BString> = state.delta.deleted.iter().collect();
        let old = index_entries(&index);
        let kept = carried(&head, &target, &old, &dirty).map_err(|lost| {
            said::would_overwrite(
                "Your local changes to the following files",
                &lost,
                "checkout",
                "Please commit your changes or stash them before you switch branches.",
            )
        })?;
        let staged = by_path(&old);
        let mut in_the_way = Vec::new();
        let tracked = |path: &[u8]| staged.contains_key(&BString::from(path));
        for entry in kept.iter().filter(|e| !staged.contains_key(&e.path)) {
            // nothing is beneath a tracked file or symlink that gives way
            let beneath_tracked = self.validate_path_among(&entry.path, &tracked)?;
            let found = self.abs(&entry.path)?.symlink_metadata();
            if !beneath_tracked && found.is_ok_and(|m| !m.is_dir()) {
                in_the_way.push(entry.path.clone());
            }
        }
        if !in_the_way.is_empty() {
            return Err(said::would_overwrite(
                "The following untracked working tree files",
                &in_the_way,
                "checkout",
                "Please move or remove them before you switch branches.",
            ));
        }
        let changes = local_changes(&target, &kept, &dirty, &gone);
        drop(state);
        let head_ref = self.head_ref.clone();
        let head_old = self.store.refs.get(&head_ref).cloned();
        let was = self.store.refs.get(full).cloned();
        let made = new.map(|commit| RefChange {
            name: full.to_owned(),
            old: was.clone(),
            new: Some(RefTarget::Oid(commit)),
        });
        let made = made.filter(|change| change.new != was);
        let moved = RefChange {
            name: head_ref,
            old: head_old,
            new: Some(RefTarget::Symbolic(full.to_owned())),
        };
        let moved = Some(moved).filter(|change| change.new != change.old);
        let refs: Vec<RefChange> = made.into_iter().chain(moved).collect();
        self.commit_refs_checkout("switch", &refs, &old, &kept)?;
        Ok(changes)
    }
}
