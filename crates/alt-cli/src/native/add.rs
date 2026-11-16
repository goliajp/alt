//! `alt add`: stage working-tree files into the index.

use std::collections::{HashMap, HashSet};

use alt_worktree::{Delta, FileStat, Snapshot, Untracked};

use super::*;

mod plan;

use plan::{plan, present};

/// Whether `path` is what `spec` names or lies beneath it; the empty spec
/// is the whole tree.
fn under(path: &[u8], spec: &str) -> bool {
    spec.is_empty()
        || path == spec.as_bytes()
        || (path.starts_with(spec.as_bytes()) && path.get(spec.len()) == Some(&b'/'))
}

/// A path stands for itself and every file under it, deleted ones included
/// (their removal is staged), as in git; a conflicted path is one the index
/// knows, with or without its file. A path that names nothing is an error.
fn all_match(specs: &[&str], given: &[String], before: &Index, delta: &Delta) -> Res<()> {
    for (spec, given) in specs.iter().zip(given).filter(|(spec, _)| !spec.is_empty()) {
        let tracked = before.entries.iter().any(|e| under(&e.path, spec));
        let seen = delta
            .untracked
            .iter()
            .chain(&delta.unmerged)
            .any(|p| under(p, spec));
        if !tracked && !seen {
            return Err(format!("pathspec '{given}' did not match any files").into());
        }
    }
    Ok(())
}

impl NativeRepo<'_> {
    /// `alt add <paths>`: stage the given paths (or everything for `.`),
    /// updating the index to match the working tree.
    pub fn add(&mut self, paths: &[String], json: bool, out: &mut impl Write) -> Res<()> {
        self.stage(paths, false, json, out)
    }

    /// `alt add --renormalize <paths>`, as in git: every tracked file
    /// among the paths is stored again the way the attributes and settings
    /// now in force clean it, whatever the index holds for it. A file that
    /// kept its stat is not looked at otherwise, so a change of the rules
    /// alone reaches the index only this way. Untracked files, conflicted
    /// paths and submodules are left as they are, and a tracked file that
    /// is gone is an error.
    pub fn renormalize(&mut self, paths: &[String], json: bool, out: &mut impl Write) -> Res<()> {
        if paths.is_empty() {
            writeln!(out, "Nothing specified, nothing added.")?;
            return Ok(());
        }
        self.stage(paths, true, json, out)
    }

    fn stage(
        &mut self,
        paths: &[String],
        renormalize: bool,
        json: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        self.ensure_writable("add")?;
        self.lock_workspace()?;
        self.ensure_no_pending_recovery()?;
        let before = self.index()?;
        let specs: Vec<&str> = paths.iter().map(|p| pathspec(p)).collect();
        self.ensure_reachable(&specs, paths, &before)?;
        let within: Vec<&[u8]> = match specs.iter().any(|p| p.is_empty()) {
            true => Vec::new(),
            false => specs.iter().map(|s| s.as_bytes()).collect(),
        };
        let named = |path: &[u8]| specs.iter().any(|spec| under(path, spec));

        // only what differs from the index is read: a file with the stat
        // it had when it was staged holds what the index holds
        let mut snapshot = self.worktree_snapshot()?;
        let (mut delta, clock) = self.scan_delta_within(&mut snapshot, Untracked::All, &within)?;
        // a repository of its own in the tree is shown as a directory and
        // left where it is
        delta.untracked.retain(|path| !path.ends_with(b"/"));
        all_match(&specs, paths, &before, &delta)?;

        let plan = plan(&before, &delta, renormalize, &named);
        // path gate: deny staging any path the policy excludes, *before*
        // writing a blob into the odb, so a denial has no side effect
        if !self.caps.path_allow.is_empty() {
            let gone: HashSet<&[u8]> = plan.deleted.iter().map(|p| p.as_slice()).collect();
            let stays = |e: &&IndexEntry| !gone.contains(e.path.as_slice());
            let present = |e: &&IndexEntry| present(e, &delta, &named);
            for e in before.entries.iter().filter(present).filter(stays) {
                self.ensure_path_allowed(e.path.to_str().unwrap_or(""))?;
            }
        }
        // stored anew, a file is cleaned without regard to what is staged
        let none = Index {
            version: 2,
            entries: Vec::new(),
            extensions: Vec::new(),
        };
        let staged_now = if renormalize { &none } else { &before };
        let mut entries = plan.entries;
        let stats = self.stage_files(&plan.taken, &delta, staged_now, &mut entries)?;
        self.store.odb.flush()?;
        entries.sort_by(|a, b| a.path.cmp(&b.path).then(a.stage().cmp(&b.stage())));
        let after = Index {
            version: 2,
            entries,
            extensions: Vec::new(),
        };
        let staged = plan.staged;
        let touched = plan.touched;
        self.put_staged(before, after, &touched, &snapshot, stats, &clock)?;
        if json {
            crate::json::emit(out, vec![("staged", crate::json::Json::Num(staged as i64))])?;
        } else {
            writeln!(out, "staged {staged} path(s)")?;
        }
        Ok(())
    }

    /// Puts the index `after` in place of `before` where they differ, and
    /// keeps the snapshot of what is then in place, with the stat each
    /// staged file had before it was read.
    fn put_staged(
        &mut self,
        before: Index,
        after: Index,
        touched: &[BString],
        snapshot: &Snapshot,
        stats: Vec<(BString, FileStat)>,
        clock: &alt_worktree::Clock,
    ) -> Res<()> {
        if after.entries == before.entries {
            self.keep_snapshot(snapshot);
            return Ok(());
        }
        let mut next = Snapshot::of_index(&after, &[], self.store.algo, Some(snapshot));
        self.write_index_transaction(before, after, touched, None)?;
        let raw = self.store.algo.raw_len();
        next.index_id = tree_state::index_checksum(&self.index_path, raw)?.unwrap_or_default();
        for (path, stat) in stats {
            next.settle_path(&path, stat, clock);
        }
        next.fill_trees();
        self.keep_snapshot(&next);
        Ok(())
    }

    /// Refuses a path named through a symlink, which is not a path of this
    /// tree, and one inside a submodule, which is the submodule's to add.
    fn ensure_reachable(&self, specs: &[&str], given: &[String], before: &Index) -> Res<()> {
        let refuse = |why: String| -> Res<()> {
            Err(crate::machine::Failure::refusal("precondition_failed", why).into())
        };
        for (spec, given) in specs.iter().zip(given) {
            let mut parents = spec.match_indices('/').map(|(at, _)| &spec[..at]);
            if parents.any(|p| self.root.join(p).is_symlink()) {
                return refuse(format!("pathspec '{given}' is beyond a symbolic link"));
            }
            let inside = before.entries.iter().find(|e| {
                e.mode == 0o160000
                    && spec
                        .as_bytes()
                        .strip_prefix(e.path.as_slice())
                        .is_some_and(|rest| rest.starts_with(b"/"))
            });
            if let Some(sub) = inside {
                return refuse(format!("pathspec '{given}' is in submodule '{}'", sub.path));
            }
        }
        Ok(())
    }

    /// Stores what the working tree has at each of `taken` and pushes its
    /// entry; returns the stat each file had before it was read.
    fn stage_files(
        &mut self,
        taken: &[&BString],
        delta: &Delta,
        before: &Index,
        entries: &mut Vec<IndexEntry>,
    ) -> Res<Vec<(BString, FileStat)>> {
        let lfs = self.lfs_rules()?;
        let attrs = self.converter()?;
        let moved: HashMap<&[u8], &WorkEntry> = delta
            .changed
            .iter()
            .filter(|w| w.mode == 0o160000)
            .map(|w| (w.path.as_slice(), w))
            .collect();
        let unread = ObjectId::hash_object(self.store.algo, ObjectKind::Blob, b"");
        let mut stats = Vec::new();
        for &path in taken {
            self.ensure_path_allowed(path.to_str().unwrap_or(""))?;
            // a submodule is recorded by the commit it has checked out,
            // which lives in its own store
            if let Some(w) = moved.get(path.as_slice()) {
                entries.push(staged_entry(path, w.oid, w.mode));
                continue;
            }
            let meta = std::fs::symlink_metadata(self.abs(path)?)
                .map_err(|e| format!("unable to stat '{path}': {e}"))?;
            let stat = FileStat::of(&meta);
            // the id is known once the stored form is: not used until then
            let file = WorkEntry {
                path: path.clone(),
                oid: unread,
                mode: stat.git_mode(),
            };
            let bytes = self.staged_bytes(&file, lfs.as_ref(), &attrs, before)?;
            let oid = ObjectId::hash_object(self.store.algo, ObjectKind::Blob, &bytes);
            self.store.odb.put(oid, ObjectKind::Blob, &bytes)?;
            entries.push(staged_entry(path, oid, file.mode));
            stats.push((path.clone(), stat));
        }
        Ok(stats)
    }
}

/// An index entry for staged content. The index keeps no stat: what the
/// working-tree file looked like is the snapshot's to remember.
fn staged_entry(path: &BString, oid: ObjectId, mode: u32) -> IndexEntry {
    IndexEntry {
        ctime: (0, 0),
        mtime: (0, 0),
        dev: 0,
        ino: 0,
        mode,
        uid: 0,
        gid: 0,
        size: 0,
        oid,
        flags: path.len().min(0x0FFF) as u16,
        extended_flags: None,
        path: path.clone(),
    }
}

/// A path argument relative to the working-tree root, without `./` in front
/// or `/` behind; the empty string means the whole tree.
fn pathspec(given: &str) -> &str {
    let spec = given.trim_start_matches("./").trim_end_matches('/');
    if spec == "." { "" } else { spec }
}
