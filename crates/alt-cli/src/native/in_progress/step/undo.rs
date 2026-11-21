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
        // each of the four as the step left it, or put back by an undo that
        // was cut short: one the step does not tell of is not there
        for name in STATE_FILES {
            let told = step.state.iter().find(|file| file.name == name);
            let now = self.state_file(name)?;
            let as_left = match told {
                Some(file) => now == file.after || now == file.before,
                None => now.is_none(),
            };
            if !as_left {
                let why = "recovery precondition failed: operation state changed";
                return Err(refusal(why));
            }
        }
        if step
            .state
            .iter()
            .any(|file| !STATE_FILES.contains(&file.name.as_str()))
        {
            return Err("corrupt step record".into());
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
        // refused here, before anything is put back, where the ref store
        // would refuse the changes
        self.ensure_no_force(&back)?;
        let checkout = step.checkout.as_ref();
        let checkout = checkout.map(|c| self.inverse_checkout(c)).transpose()?;
        // an index in conflict that the step does not tell of is another
        // operation's
        if checkout.is_none() && self.index()?.entries.iter().any(|e| e.stage() > 0) {
            return Err(refusal("recovery precondition failed: index changed"));
        }
        Ok((step, back, checkout))
    }

    fn set_state_file(&self, name: &str, text: &Option<String>) -> Res<()> {
        match text {
            Some(text) => durable_write(&self.state_path(name), text.as_bytes()),
            None => self.drop_state_file(name),
        }
    }

    /// Moves the refs back, or where there are none records the checkout
    /// to come: the one point an undo is done or not done at.
    fn begin_undo(
        &mut self,
        op: &alt_oplog::Op,
        back: &[RefChange],
        checkout: Option<&CheckoutIntent>,
    ) -> Res<Option<OpId>> {
        if back.is_empty() {
            let Some(checkout) = checkout else {
                return Ok(None);
            };
            let actor = self.id.actor("undo");
            let payload = encode(PREPARE_CHECKOUT, checkout)?;
            return Ok(Some(self.store.refs.record_op(
                &actor,
                now_ms(),
                &payload,
            )?));
        }
        self.inverse_of = Some(op.id.to_string());
        self.checkout_intent = checkout.cloned();
        let moved = match checkout.is_some() {
            true => self.commit_refs("undo", back),
            false => self.commit_refs_only("undo", back),
        };
        self.inverse_of = None;
        self.checkout_intent = None;
        Ok(Some(moved?))
    }

    /// Takes back the command `op` is the record of, and records that as
    /// a step in turn.
    pub(in crate::native) fn undo_step(&mut self, op: &alt_oplog::Op) -> Res<()> {
        let (step, back, checkout) = self.step_to_undo(op)?;
        // a state file that comes back is written before the refs and the
        // files, one that goes is removed after them: cut short anywhere,
        // the undo leaves an operation that says it is under way
        let (comes, goes): (Vec<&StateFile>, Vec<&StateFile>) =
            step.state.iter().partition(|file| file.before.is_some());
        for file in &comes {
            self.set_state_file(&file.name, &file.before)?;
        }
        let begun = match self.begin_undo(op, &back, checkout.as_ref()) {
            Ok(begun) => begun,
            Err(refused) => {
                // nothing was moved: the files are again as the step left them
                for file in &comes {
                    self.set_state_file(&file.name, &file.after)?;
                }
                return Err(refused);
            }
        };
        if let (Some(operation), Some(checkout)) = (begun, &checkout) {
            self.apply_prepared_checkout(operation, checkout)?;
        }
        for file in &goes {
            self.drop_state_file(&file.name)?;
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
