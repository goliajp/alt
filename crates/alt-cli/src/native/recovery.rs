//! Workspace-scoped recovery. Intent is durable before the index is replaced;
//! completion is a separate append. Missing completion is never success.

use super::*;

mod undo;

pub(super) const PREPARE: u8 = 0x30;
pub(super) const COMPLETE: u8 = 0x31;

#[derive(Clone, Debug, PartialEq, Eq, alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub(super) struct WorkspaceIdentity {
    pub name: String,
    pub instance: String,
}

/// Recovery semantics are written by the operation, never inferred from an
/// actor label or from the absence of a checkout in historical records.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, alt_json::ToJson, alt_json::FromJson)]
#[json(rename_all = "snake_case")]
pub(super) enum RefEffects {
    #[default]
    Unknown,
    RefsOnly,
    Checkout,
}

#[derive(alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub(super) struct RefContext {
    pub version: u32,
    #[json(default)]
    pub effects: RefEffects,
    pub workspace: WorkspaceIdentity,
    pub inverse_of: Option<String>,
    #[json(default)]
    pub checkout: Option<checkout_recovery::CheckoutIntent>,
    #[json(default)]
    pub request_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
struct PathGuard {
    path: Vec<u8>,
    state: Option<(u32, String)>,
}

#[derive(Clone, alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub(super) struct IndexIntent {
    version: u32,
    pub(super) workspace: WorkspaceIdentity,
    #[json(default)]
    pub(super) request_id: Option<String>,
    before: String,
    after: String,
    worktree: Vec<PathGuard>,
    inverse_of: Option<String>,
}

#[derive(alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
struct Completion {
    version: u32,
    prepared: String,
    #[json(default)]
    request_id: Option<String>,
}

pub(super) fn encode<T: alt_json::ToJson>(kind: u8, value: &T) -> Res<Vec<u8>> {
    let mut bytes = vec![kind, 1];
    bytes.extend(alt_json::to_vec(value)?);
    Ok(bytes)
}

pub(super) fn decode<T: alt_json::FromJson>(bytes: &[u8], kind: u8) -> Res<T> {
    if bytes.get(..2) != Some(&[kind, 1]) {
        return Err(crate::machine::Failure::refusal(
            "unsupported_record",
            "unsupported recovery record version",
        )
        .into());
    }
    Ok(alt_json::from_slice(&bytes[2..])?)
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

    /// The working-tree state at `path`, as an index change records it. A
    /// path beneath a file or a symlink holds nothing; an index change
    /// writes nothing to the working tree, so nothing can pass through
    /// such a parent.
    fn path_guard(&self, path: &BString) -> Res<PathGuard> {
        if self.validate_path_among(path, &|_| true)? {
            return Ok(PathGuard {
                path: path.to_vec(),
                state: None,
            });
        }
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
            // a directory is recorded as present, never by content: a
            // submodule placeholder, or a tracked file replaced by a directory
            Ok(meta) if meta.is_dir() => Some((0o160000, String::new())),
            Ok(_) => return Err("recovery does not support special files".into()),
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
        index_file::ensure_sound(&after)?;
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
            self.ensure_path_allowed(&guard.path)?;
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

#[cfg(test)]
mod json_tests {
    use super::*;
    alt_json_testkit::module_fixture_tests!("native-recovery"; WorkspaceIdentity, RefEffects, RefContext, PathGuard, IndexIntent, Completion);
    alt_json_testkit::module_read_tests!("state-recovery"; RefContext: RefContext, IndexIntent: IndexIntent, Completion: Completion);
}
