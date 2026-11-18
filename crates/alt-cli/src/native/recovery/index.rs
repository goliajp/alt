//! Completing an index change whose completion was never recorded.

use super::*;

impl NativeRepo<'_> {
    /// Refuses an index that names a blob which is not stored: the
    /// operation that wrote those blobs was interrupted before they were
    /// synced, and the index it prepared is not one to complete it with.
    fn ensure_blobs_stored(&self, index: &[u8]) -> Res<()> {
        for entry in Index::parse(index, self.store.algo()?)?.entries {
            let gitlink = entry.mode & 0o170000 == 0o160000;
            if !gitlink && !self.store.odb.contains(&entry.oid)? {
                return Err(crate::machine::Failure::refusal(
                    "precondition_failed",
                    format!(
                        "recovery refused: index entry {} names blob {}, which is not stored",
                        entry.path, entry.oid
                    ),
                )
                .into());
            }
        }
        Ok(())
    }

    pub(in crate::native) fn recover_index(
        &mut self,
        operation: OpId,
        intent: IndexIntent,
    ) -> Res<()> {
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
        self.ensure_blobs_stored(&after)?;
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
