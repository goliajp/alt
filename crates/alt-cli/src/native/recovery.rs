//! Workspace-scoped recovery. Intent is durable before the index is replaced;
//! completion is a separate append. Missing completion is never success.

use super::*;
use serde::{Deserialize, Serialize};

pub(super) const PREPARE: u8 = 0x30;
pub(super) const COMPLETE: u8 = 0x31;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WorkspaceIdentity {
    pub name: String,
    pub instance: String,
}

/// Recovery semantics are written by the operation, never inferred from an
/// actor label or from the absence of a checkout in historical records.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum RefEffects {
    #[default]
    Unknown,
    RefsOnly,
    Checkout,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RefContext {
    pub version: u32,
    #[serde(default)]
    pub effects: RefEffects,
    pub workspace: WorkspaceIdentity,
    pub inverse_of: Option<String>,
    #[serde(default)]
    pub checkout: Option<checkout_recovery::CheckoutIntent>,
    #[serde(default)]
    pub request_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PathGuard {
    path: Vec<u8>,
    state: Option<(u32, String)>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct IndexIntent {
    version: u32,
    pub(super) workspace: WorkspaceIdentity,
    #[serde(default)]
    pub(super) request_id: Option<String>,
    before: String,
    after: String,
    worktree: Vec<PathGuard>,
    inverse_of: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Completion {
    version: u32,
    prepared: String,
    #[serde(default)]
    request_id: Option<String>,
}

pub(super) fn encode<T: Serialize>(kind: u8, value: &T) -> Res<Vec<u8>> {
    let mut bytes = vec![kind, 1];
    bytes.extend(serde_json::to_vec(value)?);
    Ok(bytes)
}

pub(super) fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8], kind: u8) -> Res<T> {
    if bytes.get(..2) != Some(&[kind, 1]) {
        return Err(crate::machine::Failure::refusal(
            "unsupported_record",
            "unsupported recovery record version",
        )
        .into());
    }
    Ok(serde_json::from_slice(&bytes[2..])?)
}

/// Cache/stat fields are not state. Conflict stages, flags, paths, oid and
/// modes are state and must survive a restore exactly.
pub(super) fn canonical_index(mut index: Index, algo: HashAlgo) -> Vec<u8> {
    for e in &mut index.entries {
        e.ctime = (0, 0);
        e.mtime = (0, 0);
        e.dev = 0;
        e.ino = 0;
        e.uid = 0;
        e.gid = 0;
        e.size = 0;
    }
    index.serialize(algo)
}

pub(super) fn hash(bytes: &[u8]) -> String {
    alt_hash::blake3::hash(bytes).to_hex().to_string()
}

pub(super) fn durable_write(path: &Path, bytes: &[u8]) -> Res<()> {
    let parent = path.parent().ok_or("state path has no parent")?;
    std::fs::create_dir_all(parent)?;
    let mut tmp = alt_os::temp::NamedTempFile::new_in(parent)?;
    tmp.write_all(bytes)?;
    tmp.as_file().sync_all()?;
    tmp.persist(path)?;
    // Canonicalize the existing directory: Path::parent of a one-component
    // relative path is the empty path, which cannot be opened for fsync.
    let parent = std::fs::canonicalize(parent)?;
    for directory in parent.ancestors().take(3) {
        std::fs::File::open(directory)?.sync_all()?;
    }
    Ok(())
}

pub(super) fn workspace_identity_at(index_path: &Path, name: &str) -> Res<WorkspaceIdentity> {
    let path = index_path.with_file_name("workspace-id");
    let instance = match std::fs::read_to_string(&path) {
        Ok(id) => id,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            use std::io::Read;
            let mut random = [0; 32];
            std::fs::File::open("/dev/urandom")?.read_exact(&mut random)?;
            let id = hash(&random);
            let parent = path.parent().ok_or("workspace has no control directory")?;
            let mut tmp = alt_os::temp::NamedTempFile::new_in(parent)?;
            tmp.write_all(id.as_bytes())?;
            tmp.as_file().sync_all()?;
            match tmp.persist_noclobber(&path) {
                Ok(_) => std::fs::File::open(parent)?.sync_all()?,
                Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e.into()),
            }
            std::fs::read_to_string(&path)?
        }
        Err(e) => return Err(e.into()),
    };
    if instance.len() != 64 || !instance.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("invalid workspace instance identity".into());
    }
    Ok(WorkspaceIdentity {
        name: name.to_owned(),
        instance,
    })
}

impl NativeRepo<'_> {
    /// One OS lock per workspace, retained for this view's lifetime. Separate
    /// workspaces continue independently; shared refs still use their CAS lock.
    pub(super) fn lock_workspace(&mut self) -> Res<()> {
        if self.write_lock.is_none() {
            let path = self.index_path.with_file_name("write.lock");
            let file = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(path)?;
            #[cfg(unix)]
            {
                loop {
                    match alt_os::sys::flock(&file, alt_os::sys::LOCK_EX) {
                        Ok(()) => break,
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                        Err(e) => return Err(e.into()),
                    }
                }
            }
            #[cfg(not(unix))]
            return Err("workspace mutations require a supported Unix filesystem".into());
            self.write_lock = Some(file);
            self.store.refresh()?;
            self.caps = self.store.capabilities_for(&self.id.principal);
        }
        Ok(())
    }

    pub(super) fn workspace_identity(&self) -> Res<WorkspaceIdentity> {
        workspace_identity_at(&self.index_path, &self.workspace)
    }

    pub(super) fn snapshot(&self, bytes: &[u8]) -> Res<String> {
        let id = hash(bytes);
        let path = self.store.alt_dir.join("recovery/objects").join(&id);
        if !path.exists() {
            durable_write(&path, bytes)?;
        }
        if hash(&std::fs::read(path)?) != id {
            return Err("corrupt recovery snapshot".into());
        }
        Ok(id)
    }

    pub(super) fn read_snapshot(&self, id: &str) -> Res<Vec<u8>> {
        if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("invalid recovery snapshot identity".into());
        }
        let bytes = std::fs::read(self.store.alt_dir.join("recovery/objects").join(id))?;
        if hash(&bytes) != id {
            return Err("corrupt recovery snapshot".into());
        }
        Ok(bytes)
    }

    fn path_guard(&self, path: &BString) -> Res<PathGuard> {
        self.validate_recovery_path(path)?;
        let abs = self.abs(path)?;
        let state = match std::fs::symlink_metadata(&abs) {
            Ok(meta) if meta.file_type().is_symlink() => {
                use std::os::unix::ffi::OsStrExt;
                let target = std::fs::read_link(abs)?;
                Some((0o120000, hash(target.as_os_str().as_bytes())))
            }
            Ok(meta) if meta.is_file() => {
                use std::os::unix::fs::PermissionsExt;
                let mode = if meta.permissions().mode() & 0o111 != 0 {
                    0o100755
                } else {
                    0o100644
                };
                Some((mode, hash(&std::fs::read(abs)?)))
            }
            Ok(_) => return Err("recovery does not support special files or directories".into()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        Ok(PathGuard {
            path: path.to_vec(),
            state,
        })
    }

    fn check_guards(&self, guards: &[PathGuard]) -> Res<()> {
        for guard in guards {
            let actual = self.path_guard(&BString::from(guard.path.clone()))?;
            if actual != *guard {
                return Err(crate::machine::Failure::refusal(
                    "precondition_failed",
                    "recovery precondition failed: working-tree path changed",
                )
                .into());
            }
        }
        Ok(())
    }

    fn pending_index(&self, identity: &WorkspaceIdentity) -> Res<Option<(OpId, IndexIntent)>> {
        let mut completed = std::collections::HashSet::new();
        for op in self.store.refs.oplog().ops().iter().rev() {
            match op.payload.first() {
                Some(&COMPLETE) => {
                    let done: Completion = decode(&op.payload, COMPLETE)?;
                    if done.version != 1 {
                        return Err(crate::machine::Failure::refusal(
                            "unsupported_record",
                            "unsupported recovery completion",
                        )
                        .into());
                    }
                    completed.insert(done.prepared);
                }
                Some(&PREPARE) => {
                    let intent: IndexIntent = decode(&op.payload, PREPARE)?;
                    if intent.version != 1 {
                        return Err(crate::machine::Failure::refusal(
                            "unsupported_record",
                            "unsupported index intent",
                        )
                        .into());
                    }
                    if intent.workspace == *identity && !completed.contains(&op.id.to_string()) {
                        return Ok(Some((op.id, intent)));
                    }
                }
                _ => {}
            }
        }
        Ok(None)
    }

    pub(super) fn ensure_no_pending_recovery(&self) -> Res<()> {
        self.ensure_no_pending_checkout()?;
        if let Some((id, _)) = self.pending_index(&self.workspace_identity()?)? {
            return Err(crate::machine::Failure::pending(
                id,
                format!(
                    "operation {id} has no durable completion; recovery required (outcome unknown)"
                ),
            )
            .into());
        }
        Ok(())
    }

    pub(super) fn write_index_transaction(
        &mut self,
        before: Index,
        after: Index,
        touched: &[BString],
        inverse_of: Option<String>,
    ) -> Res<OpId> {
        self.lock_workspace()?;
        self.ensure_no_pending_recovery()?;
        let before = canonical_index(before, self.store.algo);
        let after = canonical_index(after, self.store.algo);
        if canonical_index(self.index()?, self.store.algo) != before {
            return Err(crate::machine::Failure::refusal(
                "precondition_failed",
                "index changed while preparing operation",
            )
            .into());
        }
        let intent = IndexIntent {
            version: 1,
            workspace: self.workspace_identity()?,
            request_id: self.request_id.map(|id| crate::machine::hex(&id)),
            before: self.snapshot(&before)?,
            after: self.snapshot(&after)?,
            worktree: touched
                .iter()
                .map(|p| self.path_guard(p))
                .collect::<Res<_>>()?,
            inverse_of,
        };
        self.apply_index_intent(intent, &after)
    }

    fn apply_index_intent(&mut self, intent: IndexIntent, after: &[u8]) -> Res<OpId> {
        self.store.odb.flush()?;
        let actor = self.id.actor(if intent.inverse_of.is_some() {
            "undo"
        } else {
            "add"
        });
        let prepared = self
            .store
            .refs
            .record_op(&actor, now_ms(), &encode(PREPARE, &intent)?)?;
        // Objects and then intent must be durable before replacing the index,
        // even when the daemon normally defers synchronization.
        self.store.sink()?.fsync_all()?;
        self.check_guards(&intent.worktree)?;
        durable_write(&self.index_path, after)?;
        let done = Completion {
            version: 1,
            prepared: prepared.to_string(),
            request_id: self.request_id.map(|id| crate::machine::hex(&id)),
        };
        self.store
            .refs
            .record_op(&actor, now_ms(), &encode(COMPLETE, &done)?)?;
        Ok(prepared)
    }

    pub(super) fn recover_index(&mut self, operation: OpId, intent: IndexIntent) -> Res<()> {
        if intent.version != 1 || intent.workspace != self.workspace_identity()? {
            return Err(crate::machine::Failure::refusal(
                "precondition_failed",
                "recovery workspace instance does not match",
            )
            .into());
        }
        self.check_guards(&intent.worktree)?;
        for guard in &intent.worktree {
            self.ensure_path_allowed(
                std::str::from_utf8(&guard.path)
                    .map_err(|_| "unsupported non-UTF-8 recovery path")?,
            )?;
        }
        let before = self.read_snapshot(&intent.before)?;
        let after = self.read_snapshot(&intent.after)?;
        Index::parse(&before, self.store.algo)?;
        Index::parse(&after, self.store.algo)?;
        let current = hash(&canonical_index(self.index()?, self.store.algo));
        if current != intent.before && current != intent.after {
            return Err(crate::machine::Failure::refusal(
                "precondition_failed",
                "recovery precondition failed: index matches neither recorded state",
            )
            .into());
        }
        if current != intent.after {
            durable_write(&self.index_path, &after)?;
        }
        self.store.refs.record_op(
            &self.id.actor("recover"),
            now_ms(),
            &encode(
                COMPLETE,
                &Completion {
                    version: 1,
                    prepared: operation.to_string(),
                    request_id: self.request_id.map(|id| crate::machine::hex(&id)),
                },
            )?,
        )?;
        Ok(())
    }

    pub fn undo(&mut self, json: bool, out: &mut impl Write) -> Res<()> {
        self.ensure_writable("undo")?;
        self.lock_workspace()?;
        self.ensure_no_pending_recovery()?;
        let workspace = self.workspace_identity()?;
        let mut last = self
            .store
            .refs
            .oplog()
            .ops()
            .last()
            .ok_or_else(|| {
                crate::machine::Failure::refusal("precondition_failed", "nothing to undo")
            })?
            .clone();
        if last.payload.first() == Some(&checkout_recovery::COMPLETE_CHECKOUT) {
            let done: checkout_recovery::CheckoutDone =
                decode(&last.payload, checkout_recovery::COMPLETE_CHECKOUT)?;
            if done.version != 1 {
                return Err(crate::machine::Failure::refusal(
                    "unsupported_record",
                    "unsupported checkout completion",
                )
                .into());
            }
            last = self
                .store
                .refs
                .oplog()
                .ops()
                .iter()
                .find(|op| op.id.to_string() == done.prepared)
                .ok_or("missing checkout intent")?
                .clone();
        }
        use crate::json::Json;
        let mut fields = vec![("undone", Json::Bool(true))];
        match last.payload.first() {
            Some(&COMPLETE) => {
                let done: Completion = decode(&last.payload, COMPLETE)?;
                let prepared = self.store.refs.oplog().ops().iter()
                    .find(|op| op.id.to_string() == done.prepared).ok_or("missing recovery intent")?;
                let intent: IndexIntent = decode(&prepared.payload, PREPARE)?;
                if intent.workspace != workspace { return Err(crate::machine::Failure::refusal("precondition_failed", "last operation belongs to a different workspace instance; select its workspace").into()); }
                if intent.version != 1 || done.version != 1 { return Err(crate::machine::Failure::refusal("unsupported_record", "unsupported recovery record").into()); }
                if hash(&canonical_index(self.index()?, self.store.algo)) != intent.after {
                    return Err(crate::machine::Failure::refusal("precondition_failed", "recovery precondition failed: index changed").into());
                }
                self.check_guards(&intent.worktree)?;
                for guard in &intent.worktree {
                    let path = std::str::from_utf8(&guard.path).map_err(|_| "unsupported non-UTF-8 recovery path")?;
                    self.ensure_path_allowed(path)?;
                }
                let bytes = self.read_snapshot(&intent.before)?;
                self.read_snapshot(&intent.after)?;
                fields.push(("paths", Json::Array(intent.worktree.iter().map(|g| Json::str(&g.path)).collect())));
                let inverse = IndexIntent {
                    before: intent.after, after: intent.before,
                    inverse_of: Some(prepared.id.to_string()), request_id: self.request_id.map(|id| crate::machine::hex(&id)), ..intent
                };
                self.apply_index_intent(inverse, &bytes)?;
            }
            Some(&checkout_recovery::PREPARE_CHECKOUT) => {
                let intent: checkout_recovery::CheckoutIntent = decode(
                    &last.payload, checkout_recovery::PREPARE_CHECKOUT,
                )?;
                let inverse = self.inverse_checkout(&intent)?;
                let prepared = self.store.refs.record_op(&self.id.actor("undo"), now_ms(),
                    &encode(checkout_recovery::PREPARE_CHECKOUT, &inverse)?)?;
                self.apply_prepared_checkout(prepared, &inverse)?;
            }
            Some(&alt_refs::PAYLOAD_REF_TX) => {
                let tx = alt_refs::parse_tx(&last.payload)?.ok_or("not a ref transaction")?;
                fields.push(("refs", Json::Array(tx.changes.iter().map(|c| Json::str(&c.name)).collect())));
                self.undo_scoped_refs(&last)?;
            },
            Some(&crate::index_tx::PAYLOAD_INDEX_TX) => return Err(crate::machine::Failure::refusal("unsupported_record", "legacy operation lacks workspace ownership and recovery prestate; automatic undo refused").into()),
            _ => return Err(crate::machine::Failure::refusal("unsupported_record", "unknown or incomplete operation; automatic undo refused").into()),
        }
        if json {
            crate::json::emit(out, fields)?;
        } else {
            writeln!(out, "Undid the last operation")?;
        }
        Ok(())
    }

    fn undo_scoped_refs(&mut self, op: &alt_oplog::Op) -> Res<()> {
        let tx = alt_refs::parse_tx(&op.payload)?.ok_or("not a ref operation")?;
        let context = tx.context.ok_or_else(|| crate::machine::Failure::refusal("unsupported_record", "legacy operation lacks workspace ownership and recovery prestate; automatic undo refused"))?;
        let context: RefContext = serde_json::from_slice(&context)?;
        if context.workspace != self.workspace_identity()? {
            return Err(crate::machine::Failure::refusal(
                "precondition_failed",
                "last operation belongs to a different workspace instance or unsupported scope",
            )
            .into());
        }
        for c in &tx.changes {
            if (c.name == "HEAD" || c.name.starts_with("workspaces/")) && c.name != self.head_ref {
                return Err(crate::machine::Failure::refusal(
                    "precondition_failed",
                    "operation changes another workspace; automatic undo refused",
                )
                .into());
            }
            if self.store.refs.get(&c.name) != c.new.as_ref() {
                return Err(crate::machine::Failure::refusal(
                    "precondition_failed",
                    "recovery precondition failed: refs changed",
                )
                .into());
            }
        }
        let inverse: Vec<_> = tx
            .changes
            .iter()
            .map(|c| RefChange {
                name: c.name.clone(),
                old: c.new.clone(),
                new: c.old.clone(),
            })
            .collect();
        // Version 1 did not describe the operation's effects. In particular,
        // a missing checkout is not evidence that the workspace was untouched.
        if context.version != 2 {
            return Err(crate::machine::Failure::refusal(
                "unsupported_record",
                "legacy operation lacks explicit recovery effects; automatic undo refused",
            )
            .into());
        }
        let intent = match (context.effects, context.checkout) {
            (RefEffects::RefsOnly, None) => None,
            (RefEffects::Checkout, Some(intent)) => Some(self.inverse_checkout(&intent)?),
            _ => {
                return Err(crate::machine::Failure::refusal(
                    "unsupported_record",
                    "unknown or incomplete recovery effects; automatic undo refused",
                )
                .into());
            }
        };
        self.inverse_of = Some(op.id.to_string());
        self.checkout_intent = intent.clone();
        let result = if intent.is_some() {
            self.commit_refs("undo", &inverse)
        } else {
            self.commit_refs_only("undo", &inverse)
        };
        self.inverse_of = None;
        self.checkout_intent = None;
        let operation = result?;
        if let Some(intent) = intent {
            self.apply_prepared_checkout(operation, &intent)?;
        }
        Ok(())
    }

    pub(super) fn validate_recovery_path(&self, bytes: &BString) -> Res<()> {
        let path = std::str::from_utf8(bytes).map_err(|_| "unsupported non-UTF-8 recovery path")?;
        if path.is_empty()
            || path
                .split('/')
                .any(|c| c.is_empty() || matches!(c, "." | ".." | ".alt" | ".git"))
            || Path::new(path).is_absolute()
        {
            return Err(crate::machine::Failure::refusal(
                "precondition_failed",
                "unsafe recovery path",
            )
            .into());
        }
        let abs = self.root.join(path);
        let mut parent = abs.parent();
        while let Some(p) = parent {
            if p == self.root {
                break;
            }
            match p.symlink_metadata() {
                Ok(m) if m.file_type().is_symlink() || !m.is_dir() => {
                    return Err(crate::machine::Failure::refusal(
                        "precondition_failed",
                        "recovery path traverses an unsafe parent",
                    )
                    .into());
                }
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            parent = p.parent();
        }
        Ok(())
    }

    pub(super) fn preflight_checkout(&self, old: &[WorkEntry], target: &[WorkEntry]) -> Res<()> {
        let old_paths: std::collections::HashSet<_> = old.iter().map(|e| &e.path).collect();
        for entry in old.iter().chain(target) {
            self.validate_recovery_path(&entry.path)?;
        }
        for entry in target {
            let path = self.abs(&entry.path)?;
            self.ensure_path_allowed(
                std::str::from_utf8(&entry.path)
                    .map_err(|_| "unsupported non-UTF-8 checkout path")?,
            )?;
            if entry.mode != 0o160000 && path.symlink_metadata().is_ok_and(|m| m.is_dir()) {
                return Err(crate::machine::Failure::refusal(
                    "precondition_failed",
                    "checkout target is occupied by a directory",
                )
                .into());
            }
            if entry.mode != 0o160000
                && !old_paths.contains(&entry.path)
                && path.symlink_metadata().is_ok()
            {
                return Err(format!(
                    "untracked working tree file '{}' would be overwritten",
                    entry.path
                )
                .into());
            }
            let mut parent = path.parent();
            while let Some(p) = parent {
                if p == self.root {
                    break;
                }
                match p.symlink_metadata() {
                    Ok(m) if !m.is_dir() || m.file_type().is_symlink() => {
                        return Err(crate::machine::Failure::refusal(
                            "precondition_failed",
                            "checkout parent is not a safe directory",
                        )
                        .into());
                    }
                    Ok(_) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                }
                parent = p.parent();
            }
            if entry.mode != 0o160000 && self.store.odb.get(&entry.oid)?.is_none() {
                return Err("missing checkout object".into());
            }
        }
        for entry in old {
            self.ensure_path_allowed(
                std::str::from_utf8(&entry.path)
                    .map_err(|_| "unsupported non-UTF-8 checkout path")?,
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durable_state_write_supports_relative_control_directories() {
        let dir = alt_os::temp::tempdir_in(".").unwrap();
        let relative = Path::new(dir.path().file_name().unwrap()).join("state");
        assert!(!relative.is_absolute());
        durable_write(&relative, b"durable state").unwrap();
        assert_eq!(std::fs::read(relative).unwrap(), b"durable state");
    }
}
