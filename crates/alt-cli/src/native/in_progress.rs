//! The operation a workspace is in the middle of.

use super::*;

mod step;
pub(super) use step::{MarkSaid, STEP, STEP_MARK, Tracked, mark_said};

/// What was begun in a workspace and waits to be finished.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OperationKind {
    Merge,
    Rebase,
    CherryPick,
    Revert,
    Bisect,
}

impl OperationKind {
    pub fn name(self) -> &'static str {
        match self {
            OperationKind::Merge => "merge",
            OperationKind::Rebase => "rebase",
            OperationKind::CherryPick => "cherry-pick",
            OperationKind::Revert => "revert",
            OperationKind::Bisect => "bisect",
        }
    }
}

pub struct Operation {
    pub kind: OperationKind,
    /// the step it stands at and how many there are, where it goes
    /// through several commits
    pub step: Option<(usize, usize)>,
    /// the branch it was started on, where it took HEAD off that branch
    pub branch: Option<String>,
}

impl Operation {
    /// `{"kind": "rebase", "step": 3, "steps": 7}`; the steps only where
    /// there are several.
    pub(super) fn json(&self) -> crate::json::Json {
        use crate::json::Json;
        let mut fields = vec![("kind", Json::str(self.kind.name()))];
        if let Some((step, steps)) = self.step {
            fields.push(("step", Json::Num(step as i64)));
            fields.push(("steps", Json::Num(steps as i64)));
        }
        Json::Object(fields)
    }
}

impl NativeRepo<'_> {
    /// Which operation is under way, by the file each leaves behind while
    /// it waits.
    pub(super) fn operation_kind(&self) -> Res<Option<OperationKind>> {
        Ok(if self.merge_head()?.is_some() {
            Some(OperationKind::Merge)
        } else if self.bisect_in_progress() {
            Some(OperationKind::Bisect)
        } else if self.rebase_in_progress() {
            Some(OperationKind::Rebase)
        } else if self.revert_state()?.is_some() {
            Some(OperationKind::Revert)
        } else if self.cherry_pick_in_progress()? {
            Some(OperationKind::CherryPick)
        } else {
            None
        })
    }

    /// The operation under way with how far it has come.
    pub(super) fn operation(&self) -> Res<Option<Operation>> {
        let Some(kind) = self.operation_kind()? else {
            return Ok(None);
        };
        let mut operation = Operation {
            kind,
            step: None,
            branch: None,
        };
        match kind {
            OperationKind::Rebase => {
                let state = self.rebase_state()?.ok_or("corrupt REBASE_STATE")?;
                let done = self.commits_since(self.rebase_head()?, state.onto)?;
                let stopped = usize::from(state.current.is_some());
                operation.step = Some((done + stopped, done + stopped + state.todo.len()));
                operation.branch = Some(state.branch);
            }
            OperationKind::Revert => {
                let state = self.revert_state()?.ok_or("corrupt REVERT_STATE")?;
                let head = self.store.refs.resolve(&self.head_ref)?;
                let done = match head {
                    Some(head) => self.commits_since(head, state.orig)?,
                    None => 0,
                };
                let stopped = usize::from(state.current.is_some());
                let steps = done + stopped + state.todo.len();
                operation.step = (steps > 1).then_some((done + stopped, steps));
            }
            OperationKind::Bisect => {
                let state = self.bisect_state()?.ok_or("corrupt BISECT_STATE")?;
                operation.branch = Some(state.branch);
            }
            OperationKind::Merge | OperationKind::CherryPick => {}
        }
        Ok(Some(operation))
    }

    /// How many commits were made on top of `base` to reach `tip`, along
    /// first parents.
    fn commits_since(&self, tip: ObjectId, base: ObjectId) -> Res<usize> {
        let (mut at, mut count) = (tip, 0);
        while at != base {
            let Some(parent) = self.commit_parents(at)?.first().copied() else {
                break;
            };
            at = parent;
            count += 1;
        }
        Ok(count)
    }
}
