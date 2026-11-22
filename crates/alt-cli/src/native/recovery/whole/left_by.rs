//! What each kind of operation in the log says of the index of a
//! workspace.
//!
//! A step says it through its checkouts and ref transactions, as any
//! command does, and not through its mark, its guard or the record that
//! closes it: the guard is the intent of a checkout from and to an index
//! no workspace has, and is the index of none. A step that was given up
//! left the index as it found it, whatever its checkouts before that
//! prepared and whichever of them a record closes: giving up closes those
//! it does not go on with.

use std::collections::HashMap;

use super::*;
use checkout_recovery::{COMPLETE_CHECKOUT, CheckoutDone, CheckoutIntent, PREPARE_CHECKOUT};

/// What an operation says of the index of a workspace.
pub(super) enum Left {
    /// nothing: another workspace's, or of no workspace
    Nothing,
    /// it moved a ref of the workspace and left the index as it was
    Refs,
    /// it left the index in this snapshot, where that is recorded; and
    /// whether it moved a ref of the workspace
    Index(Option<String>, bool),
}

/// What the records after the one looked at have said: the log is walked
/// from its end.
#[derive(Default)]
pub(super) struct Later {
    /// the records that close a prepared change
    closed: HashMap<String, u8>,
    /// the mark a step that was given up began with, until it is reached
    given_up: Option<String>,
    /// what the step found, by the earliest of its checkouts so far, and
    /// whether it moved a ref
    found: (Option<String>, bool),
}

impl Later {
    /// A record that closes the change `prepared`.
    fn closes(&mut self, prepared: String, kind: u8) -> Left {
        self.closed.insert(prepared, kind);
        Left::Nothing
    }

    /// A checkout of the workspace, alone or with a ref transaction: what
    /// it says, kept for the mark where its step was given up.
    fn checkout(&mut self, id: &str, intent: CheckoutIntent, moved: bool) -> Left {
        if self.given_up.is_some() {
            self.found = (Some(intent.index_before), self.found.1 || moved);
            return Left::Nothing;
        }
        let left = match self.closed.contains_key(id) {
            true => intent.index_after,
            false => intent.index_before,
        };
        Left::Index(Some(left), moved)
    }

    /// A ref transaction of the workspace that says what it did to the
    /// index: nothing, or nothing that is recorded.
    fn refs(&mut self, known: bool) -> Left {
        match (self.given_up.is_some(), known) {
            (true, _) => {
                self.found.1 = true;
                Left::Nothing
            }
            (false, true) => Left::Refs,
            (false, false) => Left::Index(None, true),
        }
    }

    /// A mark of a step of the workspace: whether it `gave_up` the step,
    /// which one that says so and came to hold did. The mark the step
    /// began with then says what the step found.
    fn mark(&mut self, id: &str, mark: in_progress::MarkSaid, gave_up: bool) -> Left {
        if gave_up && self.given_up.is_none() {
            self.given_up = mark.begin;
            return Left::Nothing;
        }
        if self.given_up.as_deref() != Some(id) {
            return Left::Nothing;
        }
        self.given_up = None;
        match std::mem::take(&mut self.found) {
            (None, false) => Left::Nothing,
            (None, true) => Left::Refs,
            (found, moved) => Left::Index(found, moved),
        }
    }
}

impl NativeRepo<'_> {
    /// What `op` says of the index of the workspace `identity`. Walked
    /// from the end of the log: `after` are the records after it, and
    /// `later` gathers what they said.
    pub(super) fn left_by(
        &self,
        op: &alt_oplog::Op,
        after: &[alt_oplog::Op],
        identity: &WorkspaceIdentity,
        later: &mut Later,
    ) -> Res<Left> {
        let id = op.id.to_string();
        Ok(match op.payload.first() {
            Some(&kind @ (COMPLETE | ABANDON)) => {
                later.closes(decode::<Completion>(&op.payload, kind)?.prepared, kind)
            }
            Some(&kind @ COMPLETE_CHECKOUT) => {
                later.closes(decode::<CheckoutDone>(&op.payload, kind)?.prepared, kind)
            }
            Some(&PREPARE) => {
                let intent: IndexIntent = decode(&op.payload, PREPARE)?;
                if intent.workspace != *identity {
                    return Ok(Left::Nothing);
                }
                // one that was abandoned or never closed left what was there
                let left = match later.closed.get(&id) {
                    Some(&COMPLETE) => intent.after,
                    _ => intent.before,
                };
                Left::Index(Some(left), false)
            }
            Some(&PREPARE_CHECKOUT) => {
                let intent: CheckoutIntent = decode(&op.payload, PREPARE_CHECKOUT)?;
                if intent.workspace != *identity || self.is_guard(&intent)? {
                    return Ok(Left::Nothing);
                }
                later.checkout(&id, intent, false)
            }
            Some(&in_progress::STEP_MARK) => {
                let mark = in_progress::mark_said(op)?;
                if mark.workspace != *identity {
                    return Ok(Left::Nothing);
                }
                let gave_up = mark.gives_up && self.mark_held(op, after)?;
                later.mark(&id, mark, gave_up)
            }
            Some(&alt_refs::PAYLOAD_REF_TX) => {
                let Some(context) = alt_refs::parse_tx(&op.payload)?.and_then(|tx| tx.context)
                else {
                    return Ok(Left::Nothing);
                };
                let context: RefContext = alt_json::from_slice(&context)?;
                if context.workspace != *identity {
                    return Ok(Left::Nothing);
                }
                match (context.checkout, context.effects) {
                    (Some(intent), _) => later.checkout(&id, intent, true),
                    (None, effects) => later.refs(effects == RefEffects::RefsOnly),
                }
            }
            _ => Left::Nothing,
        })
    }
}
