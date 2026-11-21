//! A step that was cut short, brought to an end by the next command that
//! writes in the workspace.
//!
//! The mark of such a step is in the log and the record that closes it is
//! not. What the step did is all in the log after the mark: its ref
//! transactions, its checkouts, and what its state files became with
//! which of them. A checkout it left unfinished is finished, the state
//! files are put as the log says they are, and the record of the step is
//! written. Nothing is guessed: the step ends where its last record left
//! it, which is before it changed anything, after all it had to do, or at
//! a point its operation is gone on with from.
//!
//! Where a program that does not know the mark wrote in the workspace
//! after it, what the step left is no longer what the log tells. The mark
//! is closed, and nothing is touched.

use std::collections::BTreeMap;

use super::super::super::checkout_recovery::{CheckoutIntent, PREPARE_CHECKOUT};
use super::super::super::recovery::{RefContext, decode};
use super::super::super::*;
use super::mark::{StateText, StepMark};
use super::{STATE_FILES, STEP, STEP_MARK, StepRecord, net_checkout, net_refs, state_changes};

/// What the log tells of a step after its mark.
#[derive(Default)]
struct Told {
    refs: Vec<RefChange>,
    checkouts: Vec<CheckoutIntent>,
    /// records with a checkout that no record completes
    unfinished: Vec<alt_oplog::Op>,
    /// what state files became
    state: BTreeMap<String, Option<String>>,
    /// what they become if the next record is the one it holds with
    waiting: Vec<StateText>,
}

impl Told {
    /// A mark tells what state files become, with the next record or at
    /// once. What an earlier mark waited with has not come to hold.
    fn mark(&mut self, state: Vec<StateText>, with_effect: bool) {
        self.waiting = state;
        if !with_effect {
            self.effect();
        }
    }

    /// A ref transaction or the intent of a checkout is the next record.
    fn effect(&mut self) {
        let held = std::mem::take(&mut self.waiting).into_iter();
        self.state.extend(held.map(|f| (f.name, f.text)));
    }
}

fn unsupported() -> Box<dyn std::error::Error> {
    crate::machine::Failure::refusal("unsupported_record", "unsupported step mark").into()
}

impl NativeRepo<'_> {
    /// The mark of the last step begun in this workspace, and whether a
    /// record closes it.
    fn last_step_mark(&self) -> Res<Option<(alt_oplog::Op, StepMark, bool)>> {
        let identity = self.workspace_identity()?;
        let mut closed = std::collections::HashSet::new();
        for op in self.store.refs.oplog().ops()?.iter().rev() {
            match op.payload.first() {
                Some(&STEP) => closed.extend(decode::<StepRecord>(&op.payload, STEP)?.begin),
                Some(&STEP_MARK) => {
                    let mark: StepMark = decode(&op.payload, STEP_MARK)?;
                    if mark.version != 1 {
                        return Err(unsupported());
                    }
                    if mark.begin.is_none() && mark.workspace == identity {
                        let closed = closed.contains(&op.id.to_string());
                        return Ok(Some((op.clone(), mark, closed)));
                    }
                }
                _ => {}
            }
        }
        Ok(None)
    }

    /// The workspace of the mark `op`, whether it is closed, as the mark
    /// that begins a step is by the record of the step, and whether there
    /// is a step to bring to an end.
    pub(in crate::native) fn step_mark_info(
        &self,
        op: &alt_oplog::Op,
    ) -> Res<(Option<recovery::WorkspaceIdentity>, bool, bool)> {
        let mark: StepMark = decode(&op.payload, STEP_MARK)?;
        let id = op.id.to_string();
        let mut closed = mark.begin.is_some();
        for later in self.store.refs.oplog().ops()?.iter().rev() {
            if closed || later.id == op.id {
                break;
            }
            if later.payload.first() == Some(&STEP) {
                let step: StepRecord = decode(&later.payload, STEP)?;
                closed = step.begin.as_deref() == Some(&id);
            }
        }
        Ok((Some(mark.workspace), closed, !closed))
    }

    /// What the log tells of the step `mark` began, from the records
    /// after it; nothing where a record of this workspace that is not the
    /// step's own is among them.
    fn told_after(&self, begun: &alt_oplog::Op, mark: &StepMark) -> Res<Option<Told>> {
        let ops = self.store.refs.oplog().ops()?;
        let at = ops.iter().rposition(|op| op.id == begun.id);
        let after = &ops[at.ok_or("operation not found")? + 1..];
        let mut told = Told::default();
        told.mark(mark.state.clone(), mark.with_effect);
        for op in after {
            let (workspace, complete, _) = self.operation_info(op)?;
            if workspace.as_ref() != Some(&mark.workspace) {
                continue;
            }
            if requests::operation_request(op)? != mark.request_id {
                return Ok(None);
            }
            match op.payload.first() {
                Some(&STEP_MARK) => {
                    let later: StepMark = decode(&op.payload, STEP_MARK)?;
                    told.mark(later.state, later.with_effect);
                }
                Some(&PREPARE_CHECKOUT) => {
                    told.effect();
                    told.checkouts.push(decode(&op.payload, PREPARE_CHECKOUT)?);
                }
                Some(&alt_refs::PAYLOAD_REF_TX) => {
                    told.effect();
                    let tx = alt_refs::parse_tx(&op.payload)?.ok_or("not a ref operation")?;
                    let context = tx.context.ok_or("missing recovery scope")?;
                    let context: RefContext = alt_json::from_slice(&context)?;
                    told.refs.extend(tx.changes);
                    told.checkouts.extend(context.checkout);
                }
                _ => return Ok(None),
            }
            if !complete {
                told.unfinished.push(op.clone());
            }
        }
        Ok(Some(told))
    }

    /// Brings a step of this workspace that was cut short to an end.
    pub(in crate::native) fn recover_open_step(&mut self) -> Res<()> {
        // the step this command is itself in the middle of is not cut short
        let in_step = |tracked: &super::Tracked| tracked.begin.is_some();
        if self.step.borrow().as_ref().is_some_and(in_step) {
            return Ok(());
        }
        let Some((begun, mark, false)) = self.last_step_mark()? else {
            return Ok(());
        };
        let mut step = StepRecord {
            version: 1,
            workspace: mark.workspace.clone(),
            request_id: mark.request_id.clone(),
            inverse_of: mark.inverse_of.clone(),
            begin: Some(begun.id.to_string()),
            abandoned: false,
            refs: Vec::new(),
            checkout: None,
            state: Vec::new(),
        };
        let Some(told) = self.told_after(&begun, &mark)? else {
            step.abandoned = true;
            let said = format!(
                "warning: operation {} was interrupted and another program has written since: \
                 it is left as it is; see `alt status`\n",
                begun.id
            );
            crate::diagnostics::emit(said.as_bytes())?;
            return self.record_step("recover", &step);
        };
        let before: Vec<Option<String>> = mark.before.iter().map(|f| f.text.clone()).collect();
        if before.len() != STATE_FILES.len() {
            return Err("corrupt step mark".into());
        }
        // the log is synced before a file is put as it says, and the state
        // files are put so before a checkout is finished, as the command
        // would have done
        self.store.sink()?.fsync_all()?;
        for (name, before) in STATE_FILES.iter().zip(&before) {
            let becomes = told.state.get(*name).unwrap_or(before);
            if self.state_file(name)? != *becomes {
                self.set_state_file(name, becomes)?;
            }
        }
        for op in &told.unfinished {
            self.finish_prepared(op)?;
        }
        step.refs = net_refs(&told.refs);
        step.checkout = net_checkout(told.checkouts);
        step.state = state_changes(&before, &told.state);
        self.record_step("recover", &step)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, text: Option<&str>) -> Vec<StateText> {
        vec![StateText {
            name: name.to_owned(),
            text: text.map(str::to_owned),
        }]
    }

    #[test]
    fn a_mark_holds_with_the_record_that_comes_next_or_not_at_all() {
        let mut told = Told::default();
        // cut short between the mark and the record it holds with
        told.mark(file("MERGE_HEAD", Some("a")), true);
        assert!(told.state.is_empty());
        // a mark after it takes its place
        told.mark(file("REVERT_STATE", Some("b")), true);
        told.effect();
        assert_eq!(told.state.len(), 1);
        assert_eq!(told.state["REVERT_STATE"].as_deref(), Some("b"));
        // one that holds at once needs no record
        told.mark(file("REVERT_STATE", None), false);
        assert_eq!(told.state["REVERT_STATE"], None);
        // a record with no mark before it changes no state file
        told.effect();
        assert_eq!(told.state.len(), 1);
    }
}
