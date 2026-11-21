//! An index change whose completion was never recorded: completing it,
//! and abandoning one that cannot be completed.
//!
//! The blobs of an index change are written before its intent. Where the
//! intent is in the log and a blob of the index it prepared is not stored
//! (a power cut with fsyncs skipped, or a damaged store), the change can
//! not be completed. A cut of the log is no way out there: no boundary
//! says the intent was never synced, and ops that commands reported done
//! may follow it. The change is abandoned instead: the index is, or is put
//! back to, what it was before, and a record says so, which closes the
//! intent as a completion does. The next command that writes in the
//! workspace does this before anything else.
//!
//! An add that is not synced puts its index in place before its records
//! are on disk, so a power cut can leave an index that a later change
//! wrote beside a log that ends in an earlier, open intent. The index is
//! then neither of the two the intent recorded, and nothing of the intent
//! is left to complete: it is abandoned with the index kept as it is.

use super::*;

mod checked;

/// The payload of the record that abandons a prepared index change: a
/// [`Completion`] naming it.
pub(in crate::native) const ABANDON: u8 = 0x35;

impl NativeRepo<'_> {
    /// The first entry of `index` that names a blob which is not stored.
    fn unstored_entry(&self, index: &[u8]) -> Res<Option<(String, ObjectId)>> {
        for entry in Index::parse(index, self.store.algo()?)?.entries {
            let gitlink = entry.mode & 0o170000 == 0o160000;
            if !gitlink && !self.store.odb.contains(&entry.oid)? {
                return Ok(Some((entry.path.to_string(), entry.oid)));
            }
        }
        Ok(None)
    }

    /// Why the index `intent` prepared cannot be put in place: its
    /// snapshot does not read, or it names a blob that is not stored.
    fn uncompletable(&self, intent: &IndexIntent) -> Res<Option<String>> {
        // a snapshot that is missing or does not read is one a power cut took
        let Ok(after) = self.read_snapshot(&intent.after) else {
            return Ok(Some("the index it prepared was not kept".to_owned()));
        };
        Ok(self.unstored_entry(&after)?.map(|(path, blob)| {
            format!(
                "index entry {path} of the index it prepared names blob {blob}, which is not \
                 stored"
            )
        }))
    }

    /// Abandons the prepared index change `operation` where the index it
    /// prepared cannot be put in place, and says so on stderr; whether it
    /// did. The index is the one from before the change or the prepared
    /// one: the prepared one is put back to the one from before, where
    /// that was kept. What either names that is not stored is told like
    /// any other staged file whose blob is lost.
    fn abandon_unstorable(&mut self, operation: OpId, intent: &IndexIntent) -> Res<bool> {
        let Some(why) = self.uncompletable(intent)? else {
            return Ok(false);
        };
        let current = hash(&canonical_index(self.index()?, self.store.algo()?));
        if current == intent.after
            && let Ok(before) = self.read_snapshot(&intent.before)
        {
            durable_write(&self.index_path, &before)?;
        }
        self.record_abandoned(operation)?;
        let said = format!(
            "warning: abandoned operation {operation}: it was interrupted, and {why}: add the \
             file again\n"
        );
        Ok(crate::diagnostics::emit(said.as_bytes()).map(|()| true)?)
    }

    fn record_abandoned(&mut self, operation: OpId) -> Res<()> {
        let closed = Completion {
            version: 1,
            prepared: operation.to_string(),
            request_id: self.request_id.map(|id| crate::machine::hex(&id)),
        };
        let actor = self.id.actor("recover");
        let payload = encode(ABANDON, &closed)?;
        self.store.refs.record_op(&actor, now_ms(), &payload)?;
        Ok(())
    }

    /// Abandons the prepared index change `operation` where the index is
    /// neither the one from before it nor the one it prepared, and says
    /// so on stderr; whether it did. A later change put that index in
    /// place and its record is not in the log: the index stays.
    fn abandon_superseded(&mut self, operation: OpId, intent: &IndexIntent) -> Res<bool> {
        let current = hash(&canonical_index(self.index()?, self.store.algo()?));
        if current == intent.before || current == intent.after {
            return Ok(false);
        }
        self.record_abandoned(operation)?;
        let said = format!(
            "warning: abandoned operation {operation}: it was interrupted, and the index was \
             changed after it by a command the operation log has no record of (a power cut \
             leaves it so when the last `alt add` was not synced). What the index stages now \
             is kept: see `alt status`\n"
        );
        Ok(crate::diagnostics::emit(said.as_bytes()).map(|()| true)?)
    }

    pub(in crate::native) fn ensure_writable(&mut self, verb: &str) -> Res<()> {
        if self.caps.read_only {
            return Err(crate::machine::Failure::refusal(
                "capability_denied",
                format!("capability denied: principal cannot {verb} (read-only)"),
            )
            .into());
        }
        self.lock_workspace()?;
        if self.caps.read_only {
            return Err(crate::machine::Failure::refusal(
                "capability_denied",
                format!("capability denied: principal cannot {verb} (read-only)"),
            )
            .into());
        }
        self.ensure_no_pending_recovery()
    }

    /// Fails where an operation of this workspace was interrupted and is
    /// to be recovered first. An index change that cannot be completed is
    /// abandoned here and is in no command's way.
    ///
    /// Only the ops the workspace has not been checked against are read:
    /// where none of them is left open, how far the log reaches is kept
    /// beside the index once the command has succeeded
    /// ([`Store::keep_checked`]), and the next check starts there.
    pub(in crate::native) fn ensure_no_pending_recovery(&mut self) -> Res<()> {
        let head = self.store.refs.head_op();
        if head.is_none() || self.recovery_checked == head {
            return Ok(());
        }
        let identity = self.workspace_identity()?;
        let place = self.index_path.with_file_name(checked::NAME);
        let (read, pending) = {
            let ops = checked::unread(self.store.refs.oplog(), &place, &identity)?;
            checkout_recovery::ensure_none_pending(&ops, &identity)?;
            (!ops.is_empty(), pending_index(&ops, &identity)?)
        };
        if let Some((id, intent)) = pending
            && !self.abandon_superseded(id, &intent)?
            && !self.abandon_unstorable(id, &intent)?
        {
            return Err(crate::machine::Failure::pending(
                id,
                format!(
                    "operation {id} has no durable completion; recovery required (outcome unknown)"
                ),
            )
            .into());
        }
        if read && let Some(anchor) = self.store.refs.oplog().anchor() {
            self.store.checked = Some((place, checked::encode(&identity, &anchor)));
        }
        self.recovery_checked = self.store.refs.head_op();
        Ok(())
    }

    /// Whether the workspace and its files are as `intent` recorded them.
    fn ensure_recoverable(&self, intent: &IndexIntent) -> Res<()> {
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
        Ok(())
    }

    pub(in crate::native) fn recover_index(
        &mut self,
        operation: OpId,
        intent: IndexIntent,
    ) -> Res<()> {
        self.ensure_recoverable(&intent)?;
        if self.abandon_superseded(operation, &intent)?
            || self.abandon_unstorable(operation, &intent)?
        {
            return Ok(());
        }
        let after = self.read_snapshot(&intent.after)?;
        let current = hash(&canonical_index(self.index()?, self.store.algo()?));
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

impl Store {
    /// Writes down how far the command that just ran found the log to hold
    /// nothing its workspace left open, for the next command, when the
    /// command `succeeded`: one that fails or is refused changes no file.
    pub fn keep_checked(&mut self, succeeded: bool) {
        // caches: where one cannot be written, the next command reads more
        if let Some((place, record)) = self.checked.take()
            && succeeded
        {
            let _ = checked::keep(&place, &record);
        }
        if let Some((place, note)) = self.seen.get_mut().unwrap().take()
            && succeeded
        {
            let _ = checked::keep(&place, &note);
        }
    }
}

/// The latest index change among `ops` that the workspace `identity`
/// prepared and nothing among them closed.
fn pending_index(
    ops: &[alt_oplog::Op],
    identity: &WorkspaceIdentity,
) -> Res<Option<(OpId, IndexIntent)>> {
    let mut completed = std::collections::HashSet::new();
    for op in ops.iter().rev() {
        match op.payload.first() {
            Some(&kind @ (COMPLETE | ABANDON)) => {
                let done: Completion = decode(&op.payload, kind)?;
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
