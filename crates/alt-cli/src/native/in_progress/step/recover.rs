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
//! A checkout the step cannot be brought through, a directory or a file
//! that cannot be written in its way, leaves the step open and says so;
//! `abandon` gives such a step up.
//!
//! An alt from before steps is kept from writing while a step is open: see
//! `guard`. Where a record of this workspace that is not the step's own
//! lies after the mark all the same, what the step left may no longer be
//! what the log tells. An index change alone, with every checkout of the
//! step finished, is no such case: the state files are put as the log says
//! and the step is closed. In any other nothing is touched. The mark is
//! closed with a record that tells nothing, at once where the state files
//! are as the log says, and only by `alt operation recover` where they are
//! not: until then every command that writes is refused and says so.

use std::collections::BTreeMap;

use super::super::super::checkout_recovery::{CheckoutIntent, PREPARE_CHECKOUT};
use super::super::super::recovery::{RefContext, decode};
use super::super::super::*;
use super::mark::StepMark;
use super::{STATE_FILES, STEP, STEP_MARK, StepRecord, net_checkout, net_refs, state_changes};

/// What the log tells of a step after its mark.
#[derive(Default)]
pub(super) struct Told {
    pub(super) refs: Vec<RefChange>,
    pub(super) checkouts: Vec<CheckoutIntent>,
    /// records with a checkout that no record completes
    unfinished: Vec<alt_oplog::Op>,
    /// those of them the step was given up after: what they had written
    /// is put back and they are not gone on with
    given_up: Vec<alt_oplog::Op>,
    /// the step was given up: it ends as it began
    undone: bool,
    /// how many of the checkouts are from before it was
    pub(super) own: Option<usize>,
    /// what state files became
    pub(super) state: BTreeMap<String, Option<String>>,
    /// the guard of the step, where no record closes it
    pub(super) guard: Option<OpId>,
    /// a record of the workspace that is not the step's own came after
    /// the mark, other than an index change
    pub(super) foreign: bool,
    /// an index change that is not the step's own did
    added: bool,
}

impl Told {
    /// A mark tells what state files become where it came to `hold`, and
    /// nothing where it did not. One that gives the step up and holds:
    /// from then on no checkout from before it is unfinished.
    fn mark(&mut self, mark: StepMark, held: bool) {
        if !held {
            return;
        }
        self.state
            .extend(mark.state.into_iter().map(|f| (f.name, f.text)));
        if mark.gives_up {
            self.given_up.append(&mut self.unfinished);
            self.undone = true;
            self.own.get_or_insert(self.checkouts.len());
        }
    }
}

/// Why the step `begun` began cannot be brought to an end, and the two ways on.
fn stuck(begun: &alt_oplog::Op, why: &dyn std::error::Error) -> Box<dyn std::error::Error> {
    let id = begun.id;
    let said = format!(
        "operation {id} was interrupted and cannot be brought to an end: {why}; \
         move what is in its way aside and run the command again, or give the operation \
         up with `alt operation abandon {id}`, which with `--keep-changes` leaves a file \
         changed since as it is"
    );
    crate::machine::Failure::pending(id, said).into()
}

/// The record that closes the step `begun` began, telling of no change yet.
fn closing(begun: &alt_oplog::Op, mark: &StepMark) -> StepRecord {
    StepRecord {
        version: 1,
        workspace: mark.workspace.clone(),
        request_id: mark.request_id.clone(),
        inverse_of: mark.inverse_of.clone(),
        begin: Some(begun.id.to_string()),
        abandoned: false,
        refs: Vec::new(),
        checkout: None,
        state: Vec::new(),
    }
}

fn unsupported() -> Box<dyn std::error::Error> {
    crate::machine::Failure::refusal("unsupported_record", "unsupported step mark").into()
}

impl NativeRepo<'_> {
    /// The mark of the last step begun in this workspace, and whether a
    /// record closes it.
    pub(super) fn last_step_mark(&self) -> Res<Option<(alt_oplog::Op, StepMark, bool)>> {
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
    /// after it.
    pub(super) fn told_after(&self, begun: &alt_oplog::Op, mark: &StepMark) -> Res<Told> {
        let ops = self.store.refs.oplog().ops()?;
        let at = ops.iter().rposition(|op| op.id == begun.id);
        let after = &ops[at.ok_or("operation not found")? + 1..];
        let mut told = Told::default();
        // the command the step is of, or the one that gives it up
        let mut request = mark.request_id.clone();
        let held = self.mark_held(begun, after)?;
        told.mark(decode(&begun.payload, STEP_MARK)?, held);
        for (at, op) in after.iter().enumerate() {
            let (workspace, complete, _) = self.operation_info(op)?;
            if workspace.as_ref() != Some(&mark.workspace) {
                continue;
            }
            let kind = op.payload.first();
            if kind == Some(&STEP_MARK) {
                let later: StepMark = decode(&op.payload, STEP_MARK)?;
                request = later.request_id.clone();
                told.mark(later, self.mark_held(op, &after[at + 1..])?);
                continue;
            }
            if requests::operation_request(op)? != request {
                match kind == Some(&recovery::PREPARE) && complete {
                    true => told.added = true,
                    false => told.foreign = true,
                }
                continue;
            }
            match kind {
                Some(&PREPARE_CHECKOUT) => {
                    let intent: CheckoutIntent = decode(&op.payload, PREPARE_CHECKOUT)?;
                    if self.is_guard(&intent)? {
                        told.guard = (!complete).then_some(op.id);
                        continue;
                    }
                    told.checkouts.push(intent);
                }
                Some(&alt_refs::PAYLOAD_REF_TX) => {
                    let tx = alt_refs::parse_tx(&op.payload)?.ok_or("not a ref operation")?;
                    let context = tx.context.ok_or("missing recovery scope")?;
                    let context: RefContext = alt_json::from_slice(&context)?;
                    told.refs.extend(tx.changes);
                    told.checkouts.extend(context.checkout);
                }
                _ => told.foreign = true,
            }
            if !complete {
                told.unfinished.push(op.clone());
            }
        }
        told.foreign |= told.added && !told.unfinished.is_empty();
        Ok(told)
    }

    /// Brings a step of this workspace that was cut short to an end;
    /// `asked` by `alt operation recover`. What that writes is synced as
    /// the command of the step would have synced it, whatever the command
    /// that finds the step syncs: once the record that closes the step is
    /// on disk, the state files are as the log says.
    pub(in crate::native) fn recover_open_step(&mut self, asked: bool) -> Res<()> {
        // the step this command is itself in the middle of is not cut short
        let in_step = |tracked: &super::Tracked| tracked.begin.is_some();
        if self.step.borrow().as_ref().is_some_and(in_step) {
            return Ok(());
        }
        let Some((begun, mark, false)) = self.last_step_mark()? else {
            return Ok(());
        };
        let left = alt_os::durability::unsynced();
        let _synced = alt_os::durability::leave_unsynced(false);
        self.end_open_step(&begun, &mark, asked)?;
        // nothing waits for a sync once a command that leaves its writes
        // unsynced has ended
        match left {
            true => self.store.sink()?.fsync_all(),
            false => Ok(()),
        }
    }

    fn end_open_step(&mut self, begun: &alt_oplog::Op, mark: &StepMark, asked: bool) -> Res<()> {
        let mut step = closing(begun, mark);
        let told = self.told_after(begun, mark)?;
        let before: Vec<Option<String>> = mark.before.iter().map(|f| f.text.clone()).collect();
        if before.len() != STATE_FILES.len() {
            return Err("corrupt step mark".into());
        }
        if told.foreign {
            return self.leave_as_it_is(begun, &told, &before, step, asked);
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
            self.finish_prepared(op, true)
                .map_err(|e| stuck(begun, &*e))?;
        }
        for op in &told.given_up {
            self.record_checkout_done("recover", op.id)?;
        }
        // a step that was given up tells of no change, whatever was left
        // as it is because it was changed since
        if !told.undone {
            step.refs = net_refs(&told.refs);
            step.checkout = net_checkout(told.checkouts);
        }
        step.state = state_changes(&before, &told.state);
        if let Some(guard) = told.guard {
            self.close_guard(guard)?;
        }
        self.record_step("recover", &step)
    }
}

#[cfg(test)]
mod tests {
    use super::super::mark::StateText;
    use super::*;

    fn file(name: &str, text: Option<&str>) -> Vec<StateText> {
        vec![StateText {
            name: name.to_owned(),
            text: text.map(str::to_owned),
        }]
    }

    fn mark(state: Vec<StateText>, with_effect: bool, gives_up: bool) -> StepMark {
        StepMark {
            version: 1,
            workspace: recovery::WorkspaceIdentity {
                name: "default".to_owned(),
                instance: String::new(),
            },
            request_id: None,
            begin: None,
            inverse_of: None,
            before: Vec::new(),
            state,
            with_effect,
            gives_up,
        }
    }

    #[test]
    fn a_mark_tells_of_the_state_files_and_of_giving_up_once_it_holds_and_not_before() {
        let mut told = Told::default();
        // cut short between the mark and the record it holds with
        told.mark(mark(file("MERGE_HEAD", Some("a")), true, false), false);
        assert!(told.state.is_empty());
        told.mark(mark(file("REVERT_STATE", Some("b")), true, false), true);
        assert_eq!(told.state.len(), 1);
        assert_eq!(told.state["REVERT_STATE"].as_deref(), Some("b"));
        told.mark(mark(file("REVERT_STATE", None), false, false), true);
        assert_eq!(told.state["REVERT_STATE"], None);
        // a step is given up once the mark that says so holds, not before
        told.mark(mark(Vec::new(), true, true), false);
        assert!(!told.undone);
        told.mark(mark(Vec::new(), true, true), true);
        assert!(told.undone);
    }
}
