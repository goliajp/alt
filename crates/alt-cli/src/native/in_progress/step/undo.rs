//! Taking a step back, as a step in turn.

use super::super::super::checkout_recovery::CheckoutIntent;
use super::super::super::recovery::decode;
use super::super::super::*;
use super::{STATE_FILES, STEP, StepRecord, Tracked, read};

fn refusal(why: &str) -> Box<dyn std::error::Error> {
    crate::machine::Failure::refusal("precondition_failed", why).into()
}

impl NativeRepo<'_> {
    /// The mark of the step `op` is the record of, where the step was cut
    /// short before it changed anything.
    pub(in crate::native) fn step_without_change(&self, op: &alt_oplog::Op) -> Res<Option<String>> {
        if op.payload.first() != Some(&STEP) {
            return Ok(None);
        }
        let step: StepRecord = decode(&op.payload, STEP)?;
        let same = |file: &super::StateFile| file.before == file.after;
        let changed = !step.refs.is_empty() || step.checkout.is_some();
        let none = !changed && !step.abandoned && step.state.iter().all(same);
        Ok(step.begin.filter(|_| none))
    }

    /// The record of the step that `op`, a record of this workspace, is
    /// one of: it lies between the mark of a step and the record that
    /// closes the mark. Refused where no record closes it.
    pub(in crate::native) fn step_of(&self, op: &alt_oplog::Op) -> Res<Option<alt_oplog::Op>> {
        let ops = self.store.refs.oplog().ops()?;
        let at = ops.iter().position(|known| known.id == op.id);
        let at = at.ok_or("operation not found")?;
        let workspace = self.operation_info(op)?.0;
        let begins = |known: &&alt_oplog::Op| -> Res<bool> {
            if known.payload.first() != Some(&super::STEP_MARK) {
                return Ok(false);
            }
            let mark: super::mark::StepMark = decode(&known.payload, super::STEP_MARK)?;
            Ok(mark.begin.is_none() && Some(mark.workspace) == workspace)
        };
        let mut begun = None;
        for known in ops[..at].iter().rev() {
            if begins(&known)? {
                begun = Some(known.id.to_string());
                break;
            }
        }
        let Some(begun) = begun else {
            return Ok(None);
        };
        for (later, known) in ops.iter().enumerate().rev() {
            if known.payload.first() == Some(&STEP)
                && decode::<StepRecord>(&known.payload, STEP)?.begin.as_ref() == Some(&begun)
            {
                return Ok((later > at).then(|| known.clone()));
            }
        }
        let why = "the last operation is part of a command that was interrupted; recover it first";
        Err(refusal(why))
    }

    /// The ref changes that take back what the step `op` is the record of
    /// did to every ref but the notes ref, refused where one is no longer
    /// as the step left it; and what the step did to the notes ref, change
    /// by change. A record from before the transactions told that tells of
    /// the notes ref as of any other.
    fn refs_to_undo(
        &self,
        op: &alt_oplog::Op,
        step: &StepRecord,
    ) -> Res<(Vec<RefChange>, Vec<RefChange>)> {
        let mut notes = match step.begin {
            Some(_) => self.step_notes(op, step)?,
            None => Vec::new(),
        };
        let mut back = Vec::new();
        for moved in &step.refs {
            let (name, old, new) = (moved.name.clone(), read(&moved.old)?, read(&moved.new)?);
            if name == crate::meta::NOTES_REF {
                if step.begin.is_none() {
                    notes.push(RefChange { name, old, new });
                }
                continue;
            }
            let head = name == "HEAD" || name.starts_with("workspaces/");
            if head && name != self.head_ref {
                let why = "operation changes another workspace; automatic undo refused";
                return Err(refusal(why));
            }
            if self.store.refs.get(&name) != new.as_ref() {
                return Err(refusal("recovery precondition failed: refs changed"));
            }
            let (old, new) = (new, old);
            back.push(RefChange { name, old, new });
        }
        Ok((back, notes))
    }

    /// The step a record tells of, the ref changes that take it back and the
    /// checkout that does: refused where the workspace is no longer as the
    /// step left it.
    fn step_to_undo(
        &mut self,
        op: &alt_oplog::Op,
        step: StepRecord,
    ) -> Res<(StepRecord, Vec<RefChange>, Option<CheckoutIntent>)> {
        if step.version != 1 {
            let why = "unsupported step record";
            return Err(crate::machine::Failure::refusal("unsupported_record", why).into());
        }
        if step.workspace != self.workspace_identity()? {
            return Err(refusal(
                "last operation belongs to a different workspace instance; select its workspace",
            ));
        }
        // each of the four as the step left it: one the step does not tell
        // of is not there
        for name in STATE_FILES {
            let told = step.state.iter().find(|file| file.name == name);
            let now = self.state_file(name)?;
            let as_left = match told {
                Some(file) => now == file.after,
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
        let (mut back, notes) = self.refs_to_undo(op, &step)?;
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
        // the notes of the commits the step made are taken out last: where
        // another workspace wrote notes since, that stores a notes commit
        back.extend(self.notes_back(&notes)?);
        self.ensure_no_force(&back)?;
        Ok((step, back, checkout))
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
            return Ok(Some(self.record_checkout("undo", checkout)?));
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

    /// Takes back the command `op` is the record of, as a step in turn.
    /// Each state file is planned to be what it was before the command,
    /// and holds with the refs moved back or the checkout to come: an
    /// undo cut short is brought to its end, or has changed nothing.
    pub(in crate::native) fn undo_step(&mut self, op: &alt_oplog::Op) -> Res<()> {
        let step: StepRecord = decode(&op.payload, STEP)?;
        if step.abandoned {
            let why = "nothing to undo: the last operation was interrupted and left as it was";
            return Err(refusal(why));
        }
        let (step, back, checkout) = self.step_to_undo(op, step)?;
        let tracked = Tracked::undoing(self.state_files()?, op.id.to_string());
        *self.step.borrow_mut() = Some(tracked);
        let undone = self.put_back(op, &step, &back, checkout.as_ref());
        let ended = self.end_step();
        undone?;
        ended
    }

    fn put_back(
        &mut self,
        op: &alt_oplog::Op,
        step: &StepRecord,
        back: &[RefChange],
        checkout: Option<&CheckoutIntent>,
    ) -> Res<()> {
        for file in &step.state {
            self.plan_state(&file.name, file.before.clone())?;
        }
        if let (Some(operation), Some(checkout)) = (self.begin_undo(op, back, checkout)?, checkout)
        {
            self.apply_prepared_checkout(operation, checkout)?;
        }
        self.settle_state()
    }
}
