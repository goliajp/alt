//! Write-ahead file/index materialization. A crash leaves an explicit pending
//! intent, with immutable before/after snapshots, rather than an unlogged tree.

use super::*;
use recovery::{WorkspaceIdentity, canonical_index, decode, durable_write, encode, hash};

pub(super) const PREPARE_CHECKOUT: u8 = 0x32;
pub(super) const COMPLETE_CHECKOUT: u8 = 0x33;

#[derive(Clone, Debug, PartialEq, Eq, alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
struct FileState {
    mode: u32,
    contents: String,
}

#[derive(Clone, alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
struct FileChange {
    path: Vec<u8>,
    before: Option<FileState>,
    after: Option<FileState>,
}

#[derive(Clone, alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub(super) struct CheckoutIntent {
    version: u32,
    pub(super) workspace: WorkspaceIdentity,
    #[json(default)]
    pub(super) request_id: Option<String>,
    index_before: String,
    index_after: String,
    files: Vec<FileChange>,
}

/// A complete tracked prestate for a multi-step operation such as rebase.
/// Stored by content identity so a restarted command cannot invent a prestate.
#[derive(alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
struct WorkspaceSnapshot {
    version: u32,
    workspace: WorkspaceIdentity,
    index: String,
    files: Vec<(Vec<u8>, Option<FileState>)>,
}

#[derive(Clone, alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub(super) struct CheckoutDone {
    pub(super) version: u32,
    pub(super) prepared: String,
    #[json(default)]
    request_id: Option<String>,
}

impl NativeRepo<'_> {
    pub(crate) fn begin_workspace_write(&mut self) -> Res<()> {
        self.ensure_writable("write")?;
        self.lock_workspace()?;
        self.ensure_no_pending_recovery()
    }

    pub(super) fn ensure_no_pending_checkout(&self) -> Res<()> {
        let mut completed = std::collections::HashSet::new();
        // Identity is only allocated on writes, after the capability gate.
        let identity = self.workspace_identity()?;
        for op in self.store.refs.oplog().ops().iter().rev() {
            match op.payload.first() {
                Some(&COMPLETE_CHECKOUT) => {
                    let done: CheckoutDone = decode(&op.payload, COMPLETE_CHECKOUT)?;
                    if done.version != 1 {
                        return Err(crate::machine::Failure::refusal(
                            "unsupported_record",
                            "unsupported checkout completion",
                        )
                        .into());
                    }
                    completed.insert(done.prepared);
                }
                Some(&PREPARE_CHECKOUT) => {
                    let pending: CheckoutIntent = decode(&op.payload, PREPARE_CHECKOUT)?;
                    if pending.version != 1 {
                        return Err(crate::machine::Failure::refusal(
                            "unsupported_record",
                            "unsupported checkout intent",
                        )
                        .into());
                    }
                    if pending.workspace == identity && !completed.contains(&op.id.to_string()) {
                        return Err(crate::machine::Failure::pending(op.id, format!("operation {} has an incomplete checkout; recovery required (outcome unknown)", op.id)).into());
                    }
                }
                Some(&alt_refs::PAYLOAD_REF_TX) => {
                    if let Some(tx) = alt_refs::parse_tx(&op.payload)?
                        && let Some(context) = tx.context
                    {
                        let context: recovery::RefContext = alt_json::from_slice(&context)?;
                        if context.workspace == identity
                            && context.checkout.is_some()
                            && !completed.contains(&op.id.to_string())
                        {
                            return Err(crate::machine::Failure::pending(op.id, format!("operation {} has an incomplete ref recovery; recovery required (outcome unknown)", op.id)).into());
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn file_state(&self, path: &BString, persist: bool) -> Res<Option<FileState>> {
        use std::os::unix::{ffi::OsStrExt, fs::PermissionsExt};
        let abs = self.abs(path)?;
        match abs.symlink_metadata() {
            Ok(meta) if meta.file_type().is_symlink() => {
                let bytes = std::fs::read_link(abs)?;
                Ok(Some(FileState {
                    mode: 0o120000,
                    contents: if persist {
                        self.snapshot(bytes.as_os_str().as_bytes())?
                    } else {
                        hash(bytes.as_os_str().as_bytes())
                    },
                }))
            }
            Ok(meta) if meta.is_file() => {
                let mode = if meta.permissions().mode() & 0o111 != 0 {
                    0o100755
                } else {
                    0o100644
                };
                Ok(Some(FileState {
                    mode,
                    contents: {
                        let bytes = std::fs::read(abs)?;
                        if persist {
                            self.snapshot(&bytes)?
                        } else {
                            hash(&bytes)
                        }
                    },
                }))
            }
            Ok(meta) if meta.is_dir() => Ok(Some(FileState {
                mode: 0o160000,
                contents: String::new(),
            })),
            Ok(_) => Err("checkout encountered unsupported special file".into()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn target_state(&self, entry: &WorkEntry) -> Res<FileState> {
        if entry.mode == 0o160000 {
            return Ok(FileState {
                mode: entry.mode,
                contents: String::new(),
            });
        }
        let object = self
            .store
            .odb
            .get(&entry.oid)?
            .ok_or("missing checkout object")?;
        let bytes = if entry.mode != 0o120000 {
            match alt_lfs::Pointer::parse(&object.data) {
                Some(pointer) => self.lfs_content(&pointer)?.unwrap_or(object.data),
                None => object.data,
            }
        } else {
            object.data
        };
        Ok(FileState {
            mode: entry.mode,
            contents: self.snapshot(&bytes)?,
        })
    }

    pub(super) fn snapshot_workspace(&self) -> Res<String> {
        let index = self.index()?;
        let mut files = Vec::new();
        for entry in &index.entries {
            self.validate_recovery_path(&entry.path)?;
            files.push((entry.path.to_vec(), self.file_state(&entry.path, true)?));
        }
        let snapshot = WorkspaceSnapshot {
            version: 1,
            workspace: self.workspace_identity()?,
            index: self.snapshot(&canonical_index(index, self.store.algo))?,
            files,
        };
        self.snapshot(&alt_json::to_vec(&snapshot)?)
    }

    pub(super) fn completed_workspace_delta(&self, snapshot: &str) -> Res<CheckoutIntent> {
        let snapshot: WorkspaceSnapshot = alt_json::from_slice(&self.read_snapshot(snapshot)?)?;
        if snapshot.version != 1 || snapshot.workspace != self.workspace_identity()? {
            return Err("invalid workspace recovery snapshot".into());
        }
        Index::parse(&self.read_snapshot(&snapshot.index)?, self.store.algo)?;
        let index = self.index()?;
        let before: std::collections::BTreeMap<_, _> = snapshot.files.into_iter().collect();
        let paths: std::collections::BTreeSet<_> = before
            .keys()
            .cloned()
            .chain(index.entries.iter().map(|e| e.path.to_vec()))
            .collect();
        let mut files = Vec::new();
        for path in paths {
            let path_bytes = BString::from(path.clone());
            self.validate_recovery_path(&path_bytes)?;
            let before = before.get(&path).cloned().flatten();
            let after = self.file_state(&path_bytes, true)?;
            if before != after {
                files.push(FileChange {
                    path,
                    before,
                    after,
                });
            }
        }
        Ok(CheckoutIntent {
            version: 1,
            workspace: snapshot.workspace,
            request_id: self.request_id.map(|id| crate::machine::hex(&id)),
            index_before: snapshot.index,
            index_after: self.snapshot(&canonical_index(index, self.store.algo))?,
            files,
        })
    }

    /// A multi-step operation has already materialized this delta. Its ref
    /// transaction still needs an explicit completion; interrupted recovery
    /// can validate the recorded poststate and finish the same operation.
    pub(super) fn complete_checkout_record(&mut self, operation: OpId) -> Res<()> {
        self.store.sink()?.fsync_all()?;
        self.store.refs.record_op(
            &self.id.actor("checkout"),
            now_ms(),
            &encode(
                COMPLETE_CHECKOUT,
                &CheckoutDone {
                    version: 1,
                    prepared: operation.to_string(),
                    request_id: self.request_id.map(|id| crate::machine::hex(&id)),
                },
            )?,
        )?;
        Ok(())
    }

    /// Invert the recorded workspace delta, after validating its entire
    /// poststate. Resolve no files from the new or old HEAD trees.
    pub(super) fn inverse_checkout(&self, intent: &CheckoutIntent) -> Res<CheckoutIntent> {
        if intent.version != 1 || intent.workspace != self.workspace_identity()? {
            return Err(crate::machine::Failure::refusal(
                "unsupported_record",
                "invalid checkout recovery scope",
            )
            .into());
        }
        for id in [&intent.index_before, &intent.index_after] {
            Index::parse(&self.read_snapshot(id)?, self.store.algo)?;
        }
        if hash(&canonical_index(self.index()?, self.store.algo)) != intent.index_after {
            return Err(crate::machine::Failure::refusal(
                "precondition_failed",
                "recovery precondition failed: index changed",
            )
            .into());
        }
        for change in &intent.files {
            let path = BString::from(change.path.clone());
            self.validate_recovery_path(&path)?;
            self.ensure_path_allowed(
                std::str::from_utf8(&path).map_err(|_| "unsupported non-UTF-8 recovery path")?,
            )?;
            for state in [&change.before, &change.after].into_iter().flatten() {
                if state.mode != 0o160000 {
                    self.read_snapshot(&state.contents)?;
                }
            }
            if self.file_state(&path, false)? != change.after {
                return Err(crate::machine::Failure::refusal(
                    "precondition_failed",
                    "recovery precondition failed: working-tree path changed",
                )
                .into());
            }
        }
        Ok(CheckoutIntent {
            version: 1,
            workspace: intent.workspace.clone(),
            request_id: self.request_id.map(|id| crate::machine::hex(&id)),
            index_before: intent.index_after.clone(),
            index_after: intent.index_before.clone(),
            files: intent
                .files
                .iter()
                .map(|change| FileChange {
                    path: change.path.clone(),
                    before: change.after.clone(),
                    after: change.before.clone(),
                })
                .collect(),
        })
    }

    pub(super) fn prepare_checkout(
        &self,
        old: &[WorkEntry],
        target: &[WorkEntry],
    ) -> Res<CheckoutIntent> {
        let targets: std::collections::BTreeMap<_, _> =
            target.iter().map(|t| (&t.path, t)).collect();
        let paths: std::collections::BTreeSet<_> =
            old.iter().chain(target).map(|e| &e.path).collect();
        let mut files = Vec::new();
        for path in paths {
            let before = self.file_state(path, true)?;
            let after = targets
                .get(path)
                .map(|t| self.target_state(t))
                .transpose()?;
            // Submodule directories are not owned by the repository checkout.
            if before.as_ref().is_some_and(|s| s.mode == 0o160000) && after.is_none() {
                continue;
            }
            if before != after {
                files.push(FileChange {
                    path: path.to_vec(),
                    before,
                    after,
                });
            }
        }
        let entries = target
            .iter()
            .map(|entry| IndexEntry {
                ctime: (0, 0),
                mtime: (0, 0),
                dev: 0,
                ino: 0,
                mode: entry.mode,
                uid: 0,
                gid: 0,
                size: 0,
                oid: entry.oid,
                flags: entry.path.len().min(0x0fff) as u16,
                extended_flags: None,
                path: entry.path.clone(),
            })
            .collect();
        let index_after = Index {
            version: 2,
            entries,
            extensions: Vec::new(),
        };
        Ok(CheckoutIntent {
            version: 1,
            workspace: self.workspace_identity()?,
            request_id: self.request_id.map(|id| crate::machine::hex(&id)),
            index_before: self.snapshot(&canonical_index(self.index()?, self.store.algo))?,
            index_after: self.snapshot(&canonical_index(index_after, self.store.algo))?,
            files,
        })
    }

    fn apply_file_state(&self, path: &BString, state: &Option<FileState>) -> Res<()> {
        let abs = self.abs(path)?;
        match state {
            None => {
                crate::durable::remove_file(&abs)?;
                if let Some(parent) = abs.parent() {
                    std::fs::File::open(parent)?.sync_all()?;
                }
                self.prune_empty_dirs(abs.parent());
            }
            Some(state) if state.mode == 0o160000 => {
                std::fs::create_dir_all(&abs)?;
                std::fs::File::open(&abs)?.sync_all()?;
                if let Some(parent) = abs.parent() {
                    std::fs::File::open(parent)?.sync_all()?;
                }
            }
            Some(state) => {
                let bytes = self.read_snapshot(&state.contents)?;
                let parent = abs.parent().ok_or("checkout path has no parent")?;
                std::fs::create_dir_all(parent)?;
                if state.mode == 0o120000 {
                    let temp = alt_os::temp::tempdir_in(parent)?;
                    let link = temp.path().join("link");
                    symlink_to(&bytes, &link)?;
                    crate::durable::rename(link, &abs)?;
                    std::fs::File::open(parent)?.sync_all()?;
                } else {
                    use std::os::unix::fs::PermissionsExt;
                    let mut tmp = alt_os::temp::NamedTempFile::new_in(parent)?;
                    tmp.write_all(&bytes)?;
                    tmp.as_file()
                        .set_permissions(std::fs::Permissions::from_mode(
                            if state.mode == 0o100755 { 0o755 } else { 0o644 },
                        ))?;
                    tmp.as_file().sync_all()?;
                    tmp.persist(&abs)?;
                    std::fs::File::open(parent)?.sync_all()?;
                }
            }
        }
        // Newly created or removed directory entries must reach disk too,
        // all the way back to the existing workspace root.
        let mut parent = abs.parent();
        while let Some(path) = parent {
            match std::fs::File::open(path) {
                Ok(dir) => dir.sync_all()?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            if path == self.root {
                break;
            }
            parent = path.parent();
        }
        Ok(())
    }

    pub(super) fn finish_interrupted_checkout(
        &mut self,
        operation: OpId,
        intent: CheckoutIntent,
    ) -> Res<()> {
        if intent.version != 1 || intent.workspace != self.workspace_identity()? {
            return Err(crate::machine::Failure::refusal(
                "precondition_failed",
                "recovery workspace instance does not match",
            )
            .into());
        }
        // Check every path and snapshot before changing the first byte.
        let mut outstanding = Vec::new();
        for change in &intent.files {
            let path = BString::from(change.path.clone());
            self.validate_recovery_path(&path)?;
            self.ensure_path_allowed(
                std::str::from_utf8(&path).map_err(|_| "unsupported non-UTF-8 recovery path")?,
            )?;
            for state in [&change.before, &change.after].into_iter().flatten() {
                if state.mode != 0o160000 {
                    self.read_snapshot(&state.contents)?;
                }
            }
            let actual = self.file_state(&path, false)?;
            if actual != change.before && actual != change.after {
                return Err(crate::machine::Failure::refusal(
                    "precondition_failed",
                    "recovery precondition failed: path matches neither recorded state",
                )
                .into());
            }
            if actual != change.after {
                outstanding.push(change);
            }
        }
        let before = self.read_snapshot(&intent.index_before)?;
        let after = self.read_snapshot(&intent.index_after)?;
        Index::parse(&before, self.store.algo)?;
        Index::parse(&after, self.store.algo)?;
        let current = hash(&canonical_index(self.index()?, self.store.algo));
        if current != intent.index_before && current != intent.index_after {
            return Err(crate::machine::Failure::refusal(
                "precondition_failed",
                "recovery precondition failed: index matches neither recorded state",
            )
            .into());
        }
        for change in outstanding {
            self.apply_file_state(&BString::from(change.path.clone()), &change.after)?;
        }
        if current != intent.index_after {
            durable_write(&self.index_path, &after)?;
        }
        self.store.refs.record_op(
            &self.id.actor("recover"),
            now_ms(),
            &encode(
                COMPLETE_CHECKOUT,
                &CheckoutDone {
                    version: 1,
                    prepared: operation.to_string(),
                    request_id: self.request_id.map(|id| crate::machine::hex(&id)),
                },
            )?,
        )?;
        Ok(())
    }

    pub(super) fn recoverable_checkout(
        &mut self,
        old: &[WorkEntry],
        target: &[WorkEntry],
    ) -> Res<()> {
        self.lock_workspace()?;
        self.ensure_no_pending_recovery()?;
        self.preflight_checkout(old, target)?;
        let intent = self.prepare_checkout(old, target)?;
        if intent.files.is_empty() && intent.index_before == intent.index_after {
            return Ok(());
        }
        let actor = self.id.actor("checkout");
        let prepared =
            self.store
                .refs
                .record_op(&actor, now_ms(), &encode(PREPARE_CHECKOUT, &intent)?)?;
        self.apply_prepared_checkout(prepared, &intent)
    }

    pub(super) fn apply_prepared_checkout(
        &mut self,
        prepared: OpId,
        intent: &CheckoutIntent,
    ) -> Res<()> {
        self.store.odb.flush()?;
        self.store.sink()?.fsync_all()?;
        // Validate the entire affected set before the first file replacement.
        for change in &intent.files {
            if self.file_state(&BString::from(change.path.clone()), false)? != change.before {
                return Err(crate::machine::Failure::refusal(
                    "precondition_failed",
                    "checkout precondition changed after preparation",
                )
                .into());
            }
        }
        if hash(&canonical_index(self.index()?, self.store.algo)) != intent.index_before {
            return Err(crate::machine::Failure::refusal(
                "precondition_failed",
                "checkout index changed after preparation",
            )
            .into());
        }
        for change in &intent.files {
            self.apply_file_state(&BString::from(change.path.clone()), &change.after)?;
        }
        durable_write(&self.index_path, &self.read_snapshot(&intent.index_after)?)?;
        self.store.refs.record_op(
            &self.id.actor("checkout"),
            now_ms(),
            &encode(
                COMPLETE_CHECKOUT,
                &CheckoutDone {
                    version: 1,
                    prepared: prepared.to_string(),
                    request_id: self.request_id.map(|id| crate::machine::hex(&id)),
                },
            )?,
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod json_tests {
    use super::*;
    alt_json_testkit::module_fixture_tests!("native-checkout_recovery"; FileState, FileChange, CheckoutIntent, WorkspaceSnapshot, CheckoutDone);
    alt_json_testkit::module_read_tests!("state-checkout"; CheckoutDone: CheckoutDone, WorkspaceSnapshot: WorkspaceSnapshot);
}
