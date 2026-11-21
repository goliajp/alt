//! Write-ahead file/index materialization. A crash leaves an explicit pending
//! intent, with immutable before/after snapshots, rather than an unlogged tree.

use super::*;
use recovery::{WorkspaceIdentity, canonical_index, decode, durable_write, encode, hash};

mod inverse;
mod prestate;

pub(super) const PREPARE_CHECKOUT: u8 = 0x32;
pub(super) const COMPLETE_CHECKOUT: u8 = 0x33;

#[derive(Clone, Debug, PartialEq, Eq, alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub(super) struct FileState {
    pub(super) mode: u32,
    pub(super) contents: String,
}

#[derive(Clone, alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub(super) struct FileChange {
    pub(super) path: Vec<u8>,
    pub(super) before: Option<FileState>,
    pub(super) after: Option<FileState>,
}

#[derive(Clone, alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub(super) struct CheckoutIntent {
    pub(super) version: u32,
    pub(super) workspace: WorkspaceIdentity,
    #[json(default)]
    pub(super) request_id: Option<String>,
    pub(super) index_before: String,
    pub(super) index_after: String,
    pub(super) files: Vec<FileChange>,
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
        for op in self.store.refs.oplog().ops()?.iter().rev() {
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
        let paths: std::collections::HashSet<&[u8]> =
            intent.files.iter().map(|c| c.path.as_slice()).collect();
        for change in &intent.files {
            let path = BString::from(change.path.clone());
            self.ensure_path_allowed(&path)?;
            for state in [&change.before, &change.after].into_iter().flatten() {
                if state.mode != 0o160000 {
                    self.read_snapshot(&state.contents)?;
                }
            }
            let actual = self.observed_among(&paths, change)?;
            // a path that changes between a submodule directory and a file
            // is absent for a moment: the one goes before the other comes
            let kinds =
                [&change.before, &change.after].map(|s| s.as_ref().map(|s| s.mode == 0o160000));
            let between = actual.is_none() && matches!(kinds, [Some(a), Some(b)] if a != b);
            if actual != change.before && actual != change.after && !between {
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
        Index::parse(&before, self.store.algo()?)?;
        Index::parse(&after, self.store.algo()?)?;
        let current = hash(&canonical_index(self.index()?, self.store.algo()?));
        if current != intent.index_before && current != intent.index_after {
            return Err(crate::machine::Failure::refusal(
                "precondition_failed",
                "recovery precondition failed: index matches neither recorded state",
            )
            .into());
        }
        self.ensure_directories_replaceable(&intent)?;
        self.apply_changes(outstanding.iter().copied())?;
        if current != intent.index_after {
            durable_write(&self.index_path, &after)?;
        }
        self.snapshot_checked_out(&after)?;
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
        self.record_and_checkout(intent)
    }

    /// Does what `intent` tells, its intent in the log first; nothing
    /// where it changes nothing.
    pub(super) fn record_and_checkout(&mut self, intent: CheckoutIntent) -> Res<()> {
        if intent.files.is_empty() && intent.index_before == intent.index_after {
            return Ok(());
        }
        let prepared = self.record_checkout("checkout", &intent)?;
        self.apply_prepared_checkout(prepared, &intent)
    }

    /// Writes the intent of a checkout to the log.
    pub(super) fn record_checkout(&mut self, verb: &str, intent: &CheckoutIntent) -> Res<OpId> {
        self.step_effect()?;
        let payload = encode(PREPARE_CHECKOUT, intent)?;
        crash_point!("checkout-intent:before");
        let actor = self.id.actor(verb);
        let prepared = self.store.refs.record_op(&actor, now_ms(), &payload)?;
        crash_point!("checkout-intent:after");
        self.step_effected();
        Ok(prepared)
    }

    pub(super) fn apply_prepared_checkout(
        &mut self,
        prepared: OpId,
        intent: &CheckoutIntent,
    ) -> Res<()> {
        self.store.odb.flush()?;
        self.store.sink()?.fsync_all()?;
        // Validate the entire affected set before the first file replacement.
        let paths: std::collections::HashSet<&[u8]> =
            intent.files.iter().map(|c| c.path.as_slice()).collect();
        for change in &intent.files {
            if self.observed_among(&paths, change)? != change.before {
                return Err(crate::machine::Failure::refusal(
                    "precondition_failed",
                    "checkout precondition changed after preparation",
                )
                .into());
            }
        }
        if hash(&canonical_index(self.index()?, self.store.algo()?)) != intent.index_before {
            return Err(crate::machine::Failure::refusal(
                "precondition_failed",
                "checkout index changed after preparation",
            )
            .into());
        }
        self.ensure_directories_replaceable(intent)?;
        self.step_checkout(intent);
        // a state file that holds with this checkout is there before its files
        self.apply_state(false)?;
        self.apply_changes(intent.files.iter())?;
        let index = self.read_snapshot(&intent.index_after)?;
        crash_point!("checkout-index:before");
        durable_write(&self.index_path, &index)?;
        self.snapshot_checked_out(&index)?;
        crash_point!("checkout-done:before");
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
