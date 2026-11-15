//! `alt add`: stage working-tree files into the index.

use super::*;

impl NativeRepo<'_> {
    /// `alt add <paths>`: stage the given paths (or everything for `.`),
    /// updating the index to match the working tree.
    pub fn add(&mut self, paths: &[String], json: bool, out: &mut impl Write) -> Res<()> {
        self.ensure_writable("add")?;
        self.lock_workspace()?;
        self.ensure_no_pending_recovery()?;
        let before = self.index()?;
        let lfs = self.lfs_rules()?;
        let attrs = self.converter()?;
        let specs: Vec<&str> = paths.iter().map(|p| pathspec(p)).collect();
        // a path named through a symlink is not a path of this tree
        for (spec, given) in specs.iter().zip(paths) {
            let parents = spec.match_indices('/').map(|(at, _)| &spec[..at]);
            if parents.into_iter().any(|p| self.root.join(p).is_symlink()) {
                return Err(crate::machine::Failure::refusal(
                    "precondition_failed",
                    format!("pathspec '{given}' is beyond a symbolic link"),
                )
                .into());
            }
        }
        // what lies inside a submodule is the submodule's to add
        for (spec, given) in specs.iter().zip(paths) {
            let inside = before.entries.iter().find(|e| {
                e.mode == 0o160000
                    && spec
                        .as_bytes()
                        .strip_prefix(e.path.as_slice())
                        .is_some_and(|rest| rest.starts_with(b"/"))
            });
            if let Some(sub) = inside {
                return Err(crate::machine::Failure::refusal(
                    "precondition_failed",
                    format!("pathspec '{given}' is in submodule '{}'", sub.path),
                )
                .into());
            }
        }
        let staging_all = specs.iter().any(|p| p.is_empty());
        let within: Vec<&[u8]> = match staging_all {
            true => Vec::new(),
            false => specs.iter().map(|s| s.as_bytes()).collect(),
        };
        let under = |path: &[u8], spec: &str| {
            spec.is_empty()
                || path == spec.as_bytes()
                || (path.starts_with(spec.as_bytes()) && path.get(spec.len()) == Some(&b'/'))
        };
        let named = |path: &[u8]| specs.iter().any(|spec| under(path, spec));

        // only what differs from the index is read: a file with the stat
        // it had when it was staged holds what the index holds
        let mut snapshot = self.worktree_snapshot()?;
        let (delta, stamp) = self.scan_delta_within(&mut snapshot, true, &within)?;
        // a path stands for itself and every file under it, deleted ones
        // included (their removal is staged), as in git; a conflicted
        // path is one the index knows, with or without its file
        for (spec, given) in specs.iter().zip(paths).filter(|(spec, _)| !spec.is_empty()) {
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

        // the paths whose entries change: taken from the working tree, or dropped
        let taken: Vec<&BString> = delta
            .changed
            .iter()
            .map(|w| &w.path)
            .chain(&delta.untracked)
            .chain(&delta.unmerged)
            .collect();
        let acted: std::collections::HashSet<&[u8]> = taken
            .iter()
            .chain(&delta.deleted.iter().collect::<Vec<_>>())
            .map(|p| p.as_slice())
            .collect();
        // a conflicted path among those named is resolved by what the
        // working tree has there: the file when there is one (`unmerged`),
        // a removal when there is none. git does the same whether the path
        // is named, lies under a named directory or the whole tree is
        // staged. The scan does not report a conflicted submodule without
        // a directory as deleted, so those are dropped here
        let resolved = |e: &&IndexEntry| e.stage() != 0 && named(&e.path);
        let mut entries: Vec<IndexEntry> = before
            .entries
            .iter()
            .filter(|e| !acted.contains(e.path.as_slice()) && !resolved(e))
            .cloned()
            .collect();
        let dropped: Vec<BString> = before
            .entries
            .iter()
            .filter(|e| resolved(e) && !acted.contains(e.path.as_slice()))
            .map(|e| e.path.clone())
            .collect();

        // path gate: deny staging any path the policy excludes, *before*
        // writing a blob into the odb, so a denial has no side effect
        if !self.caps.path_allow.is_empty() {
            let gone: std::collections::HashSet<&[u8]> =
                delta.deleted.iter().map(|p| p.as_slice()).collect();
            let kept = |e: &&IndexEntry| e.mode != 0o160000 || delta.submodules.contains(&e.path);
            for e in before.entries.iter().filter(kept) {
                if e.stage() == 0 && named(&e.path) && !gone.contains(e.path.as_slice()) {
                    self.ensure_path_allowed(e.path.to_str().unwrap_or(""))?;
                }
            }
        }
        let moved: std::collections::HashMap<&[u8], &WorkEntry> = delta
            .changed
            .iter()
            .filter(|w| w.mode == 0o160000)
            .map(|w| (w.path.as_slice(), w))
            .collect();
        let unread = ObjectId::hash_object(self.store.algo, ObjectKind::Blob, b"");
        let mut stats = Vec::new();
        for path in taken {
            self.ensure_path_allowed(path.to_str().unwrap_or(""))?;
            // a submodule is recorded by the commit it has checked out,
            // which lives in its own store
            if let Some(w) = moved.get(path.as_slice()) {
                entries.push(staged_entry(path, w.oid, w.mode));
                continue;
            }
            let stat = alt_worktree::FileStat::of(&std::fs::symlink_metadata(self.abs(path)?)?);
            // the id is known once the stored form is: not used until then
            let file = WorkEntry {
                path: path.clone(),
                oid: unread,
                mode: stat.git_mode(),
            };
            let bytes = self.staged_bytes(&file, lfs.as_ref(), &attrs, &before)?;
            let oid = ObjectId::hash_object(self.store.algo, ObjectKind::Blob, &bytes);
            self.store.odb.put(oid, ObjectKind::Blob, &bytes)?;
            entries.push(staged_entry(path, oid, file.mode));
            stats.push((path.clone(), stat));
        }
        // every file of the named part of the tree counts as staged,
        // changed or not
        let present = before
            .entries
            .iter()
            .filter(|e| e.stage() == 0 && named(&e.path) && !acted.contains(e.path.as_slice()))
            .filter(|e| e.mode != 0o160000 || delta.submodules.contains(&e.path))
            .count();
        let staged = present + delta.changed.len() + delta.untracked.len() + delta.unmerged.len();

        self.store.odb.flush()?;
        entries.sort_by(|a, b| a.path.cmp(&b.path).then(a.stage().cmp(&b.stage())));
        let after = Index {
            version: 2,
            entries,
            extensions: Vec::new(),
        };
        if after.entries != before.entries {
            let mut next =
                alt_worktree::Snapshot::of_index(&after, &[], self.store.algo, Some(&snapshot));
            let mut touched: Vec<BString> = acted
                .iter()
                .map(|p| BString::from(*p))
                .chain(dropped)
                .collect();
            touched.sort();
            touched.dedup();
            self.write_index_transaction(before, after, &touched, None)?;
            let raw = self.store.algo.raw_len();
            next.index_id = tree_state::index_checksum(&self.index_path, raw)?.unwrap_or_default();
            for (path, stat) in stats {
                if stat.settled(stamp)
                    && let Some(at) = next.find(&path)
                {
                    next.set_stat(at, Some(stat));
                }
            }
            next.fill_trees();
            self.keep_snapshot(&next);
        } else {
            self.keep_snapshot(&snapshot);
        }
        if json {
            crate::json::emit(out, vec![("staged", crate::json::Json::Num(staged as i64))])?;
        } else {
            writeln!(out, "staged {staged} path(s)")?;
        }
        Ok(())
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
