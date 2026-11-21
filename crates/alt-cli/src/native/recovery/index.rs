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

use super::*;

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

    /// Abandons the prepared index change `operation` where the index it
    /// prepared names a blob that is not stored, and says so on stderr;
    /// whether it did. The index must be the one from before the change
    /// or the prepared one, and the one from before must name only what
    /// is stored: any other state is left for `operation recover` to
    /// report.
    fn abandon_unstorable(&mut self, operation: OpId, intent: &IndexIntent) -> Res<bool> {
        let after = self.read_snapshot(&intent.after)?;
        let Some((path, blob)) = self.unstored_entry(&after)? else {
            return Ok(false);
        };
        let current = hash(&canonical_index(self.index()?, self.store.algo()?));
        if current == intent.after {
            let before = self.read_snapshot(&intent.before)?;
            if self.unstored_entry(&before)?.is_some() {
                return Ok(false);
            }
            durable_write(&self.index_path, &before)?;
        } else if current != intent.before {
            return Ok(false);
        }
        let closed = Completion {
            version: 1,
            prepared: operation.to_string(),
            request_id: self.request_id.map(|id| crate::machine::hex(&id)),
        };
        let actor = self.id.actor("recover");
        self.store
            .refs
            .record_op(&actor, now_ms(), &encode(ABANDON, &closed)?)?;
        let said = format!(
            "warning: abandoned operation {operation}: it was interrupted, and index entry {path} \
             of the index it prepared names blob {blob}, which is not stored: add the file again\n"
        );
        Ok(crate::diagnostics::emit(said.as_bytes()).map(|()| true)?)
    }

    fn pending_index(&self, identity: &WorkspaceIdentity) -> Res<Option<(OpId, IndexIntent)>> {
        let mut completed = std::collections::HashSet::new();
        for op in self.store.refs.oplog().ops()?.iter().rev() {
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

    /// Fails where an operation of this workspace was interrupted and is
    /// to be recovered first. An index change that cannot be completed is
    /// abandoned here and is in no command's way.
    pub(in crate::native) fn ensure_no_pending_recovery(&mut self) -> Res<()> {
        self.ensure_no_pending_checkout()?;
        if let Some((id, intent)) = self.pending_index(&self.workspace_identity()?)?
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
        let before = self.read_snapshot(&intent.before)?;
        let after = self.read_snapshot(&intent.after)?;
        Index::parse(&before, self.store.algo()?)?;
        Index::parse(&after, self.store.algo()?)?;
        let current = hash(&canonical_index(self.index()?, self.store.algo()?));
        if current != intent.before && current != intent.after {
            return Err(crate::machine::Failure::refusal(
                "precondition_failed",
                "recovery precondition failed: index matches neither recorded state",
            )
            .into());
        }
        if self.abandon_unstorable(operation, &intent)? {
            return Ok(());
        }
        if let Some((path, blob)) = self.unstored_entry(&after)? {
            return Err(crate::machine::Failure::refusal(
                "precondition_failed",
                format!(
                    "recovery refused: index entry {path} names blob {blob}, which is not stored"
                ),
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
