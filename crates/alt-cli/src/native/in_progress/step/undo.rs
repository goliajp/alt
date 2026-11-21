//! Taking a step back: the state files first, so that an undo cut short
//! leaves an operation that says it is under way, and then the refs, the
//! index and the files.

use super::super::super::checkout_recovery::{CheckoutIntent, PREPARE_CHECKOUT};
use super::super::super::recovery::{decode, durable_write, encode};
use super::super::super::*;
use super::{STATE_FILES, STEP, StateFile, StepRecord, net_refs, read};

fn refusal(why: &str) -> Box<dyn std::error::Error> {
    crate::machine::Failure::refusal("precondition_failed", why).into()
}

impl NativeRepo<'_> {
    /// The step `op` records, the ref changes that take it back and the
    /// checkout that does: refused where the workspace is no longer as the
    /// step left it.
    fn step_to_undo(
        &self,
        op: &alt_oplog::Op,
    ) -> Res<(StepRecord, Vec<RefChange>, Option<CheckoutIntent>)> {
        let step: StepRecord = decode(&op.payload, STEP)?;
        if step.version != 1 {
            let why = "unsupported step record";
            return Err(crate::machine::Failure::refusal("unsupported_record", why).into());
        }
        if step.workspace != self.workspace_identity()? {
            return Err(refusal(
                "last operation belongs to a different workspace instance; select its workspace",
            ));
        }
        for file in &step.state {
            if !STATE_FILES.contains(&file.name.as_str()) {
                return Err("corrupt step record".into());
            }
            // as the command left it, or put back by an undo that was cut short
            let now = self.state_file(&file.name)?;
            if now != file.after && now != file.before {
                let why = "recovery precondition failed: operation state changed";
                return Err(refusal(why));
            }
        }
        let mut back = Vec::new();
        for moved in &step.refs {
            let change = RefChange {
                name: moved.name.clone(),
                old: read(&moved.new)?,
                new: read(&moved.old)?,
            };
            let head = change.name == "HEAD" || change.name.starts_with("workspaces/");
            if head && change.name != self.head_ref {
                let why = "operation changes another workspace; automatic undo refused";
                return Err(refusal(why));
            }
            if self.store.refs.get(&change.name) != change.old.as_ref() {
                return Err(refusal("recovery precondition failed: refs changed"));
            }
            back.push(change);
        }
        let checkout = step.checkout.as_ref();
        let checkout = checkout.map(|c| self.inverse_checkout(c)).transpose()?;
        Ok((step, back, checkout))
    }

    /// Takes back the command `op` is the record of, and records that as
    /// a step in turn.
    pub(in crate::native) fn undo_step(&mut self, op: &alt_oplog::Op) -> Res<()> {
        let (step, back, checkout) = self.step_to_undo(op)?;
        for file in &step.state {
            match &file.before {
                Some(text) => durable_write(&self.state_path(&file.name), text.as_bytes())?,
                None => self.drop_state_file(&file.name)?,
            }
        }
        if !back.is_empty() {
            self.inverse_of = Some(op.id.to_string());
            self.checkout_intent = checkout.clone();
            let moved = match checkout.is_some() {
                true => self.commit_refs("undo", &back),
                false => self.commit_refs_only("undo", &back),
            };
            self.inverse_of = None;
            self.checkout_intent = None;
            let operation = moved?;
            if let Some(checkout) = &checkout {
                self.apply_prepared_checkout(operation, checkout)?;
            }
        } else if let Some(checkout) = &checkout {
            let actor = self.id.actor("undo");
            let payload = encode(PREPARE_CHECKOUT, checkout)?;
            let prepared = self.store.refs.record_op(&actor, now_ms(), &payload)?;
            self.apply_prepared_checkout(prepared, checkout)?;
        }
        let turned = |file: StateFile| StateFile {
            name: file.name,
            before: file.after,
            after: file.before,
        };
        self.record_step(&StepRecord {
            version: 1,
            workspace: step.workspace,
            request_id: self.request_id.map(|id| crate::machine::hex(&id)),
            inverse_of: Some(op.id.to_string()),
            refs: net_refs(&back),
            checkout,
            state: step.state.into_iter().map(turned).collect(),
        })
    }
}
