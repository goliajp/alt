//! `alt add`: stage working-tree files into the index.

use super::*;

impl NativeRepo<'_> {
    /// `alt add <paths>`: stage the given paths (or everything for `.`),
    /// updating the index to match the working tree.
    pub fn add(&mut self, paths: &[String], json: bool, out: &mut impl Write) -> Res<()> {
        self.ensure_writable("add")?;
        // add re-reads + re-puts every staged path anyway; the stat-cache
        // fast path would save the scan-time hash but lose to read_for +
        // odb.put on the next line. Wins for a smarter add are tracked
        // separately (the same-oid short-circuit in odb.put helps a bit).
        // the index decides which ignored paths are tracked and so still count
        let scan = self.scan_tree(&self.index()?)?;
        let lfs = self.lfs_rules()?;
        let specs: Vec<&str> = paths.iter().map(|p| pathspec(p)).collect();
        let staging_all = specs.iter().any(|p| p.is_empty());

        // snapshot the prior stage-0 entries — both the starting point for
        // the new index and the "old" side of every IndexChange we'll
        // record in the op log so `alt undo` can roll a stray `add` back.
        let prior_stage_zero: Vec<IndexEntry> = self
            .index()?
            .entries
            .into_iter()
            .filter(|e| e.stage() == 0)
            .collect();
        let mut entries: Vec<IndexEntry> = if staging_all {
            Vec::new()
        } else {
            prior_stage_zero.clone()
        };

        let mut staged = 0;
        let targets: Vec<BString> = if staging_all {
            scan.iter().map(|w| w.path.clone()).collect()
        } else {
            // a directory stands for every file under it, deleted ones
            // included (their removal is staged), as in git
            let mut targets = std::collections::BTreeSet::new();
            for (spec, given) in specs.iter().zip(paths) {
                let under = |path: &BString| {
                    path.as_slice() == spec.as_bytes()
                        || (path.starts_with(spec.as_bytes())
                            && path.get(spec.len()) == Some(&b'/'))
                };
                let matched: Vec<BString> = scan
                    .iter()
                    .map(|w| &w.path)
                    .chain(prior_stage_zero.iter().map(|e| &e.path))
                    .filter(|p| under(p))
                    .cloned()
                    .collect();
                if matched.is_empty() {
                    return Err(format!("pathspec '{given}' did not match any files").into());
                }
                targets.extend(matched);
            }
            targets.into_iter().collect()
        };
        // Touched paths are the union of "what changed in `entries`" — for
        // staging_all it's the whole prior + everything in scan; for the
        // explicit path form it's just the listed targets. We compute the
        // change list after the index is rewritten so old/new come from
        // the actual entries the index will hold.
        let touched_paths: std::collections::BTreeSet<BString> = if staging_all {
            prior_stage_zero
                .iter()
                .map(|e| e.path.clone())
                .chain(scan.iter().map(|w| w.path.clone()))
                .collect()
        } else {
            targets.iter().cloned().collect()
        };
        for rel in &targets {
            entries.retain(|e| &e.path != rel);
            if let Some(w) = scan.iter().find(|w| &w.path == rel) {
                // path gate: deny staging any path the policy excludes,
                // *before* writing the blob into the odb — so a denial costs
                // no on-disk side effect (the odb put would otherwise persist
                // before the index is even written).
                let path_str = w.path.to_str().unwrap_or("");
                self.ensure_path_allowed(path_str)?;
                let bytes = self.staged_bytes(w, lfs.as_ref())?;
                self.store.odb.put(w.oid, ObjectKind::Blob, &bytes)?;
                entries.push(self.make_entry(w)?);
                staged += 1;
            } // a path that vanished from the tree is dropped (staged deletion)
        }

        self.store.odb.flush()?;
        save_index(
            &self.index_path,
            &Index {
                version: 2,
                entries: entries.clone(),
                extensions: Vec::new(),
            },
        )?;

        // record the index delta so `alt undo` can roll an `add`
        // back. Skip when the call was a true no-op (touched zero paths or
        // every touched path's entry was unchanged) — keeps an empty add
        // from polluting the op log and wasting an undo step.
        let prior_by_path: std::collections::HashMap<&BString, &IndexEntry> =
            prior_stage_zero.iter().map(|e| (&e.path, e)).collect();
        let new_by_path: std::collections::HashMap<&BString, &IndexEntry> =
            entries.iter().map(|e| (&e.path, e)).collect();
        let mut changes = Vec::new();
        for p in &touched_paths {
            let old = prior_by_path.get(p).map(|e| (e.oid, e.mode));
            let new = new_by_path.get(p).map(|e| (e.oid, e.mode));
            if old != new {
                changes.push(crate::index_tx::IndexChange {
                    path: p.clone(),
                    old,
                    new,
                });
            }
        }
        if !changes.is_empty() {
            let payload = crate::index_tx::encode(&changes, self.store.algo);
            let actor = self.id.actor("add");
            self.store.refs.record_op(&actor, now_ms(), &payload)?;
        }
        if json {
            crate::json::emit(out, vec![("staged", crate::json::Json::Num(staged as i64))])?;
        } else {
            writeln!(out, "staged {staged} path(s)")?;
        }
        Ok(())
    }
}

/// A path argument relative to the working-tree root, without `./` in front
/// or `/` behind; the empty string means the whole tree.
fn pathspec(given: &str) -> &str {
    let spec = given.trim_start_matches("./").trim_end_matches('/');
    if spec == "." { "" } else { spec }
}
