//! `alt undo`: the last operation of this workspace taken back, the index,
//! the files and the refs it changed put as they were.

use super::*;

impl NativeRepo<'_> {
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
                    self.ensure_path_allowed(&guard.path)?;
                }
                let bytes = self.read_snapshot(&intent.before)?;
                self.read_snapshot(&intent.after)?;
                fields.extend(crate::json::paths("paths", "paths_bytes", intent.worktree.iter().map(|g| g.path.as_slice())));
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
        let context: RefContext = alt_json::from_slice(&context)?;
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
}
