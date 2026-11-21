//! What each kind of operation in the log says of the index of a
//! workspace.

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

/// What `op` says of the index of the workspace `identity`. Walked from
/// the end of the log: `closed` gathers the records that close a prepared
/// change, which come after it.
pub(super) fn left_by(
    op: &alt_oplog::Op,
    identity: &WorkspaceIdentity,
    closed: &mut HashMap<String, u8>,
) -> Res<Left> {
    let id = op.id.to_string();
    Ok(match op.payload.first() {
        Some(&kind @ (COMPLETE | ABANDON)) => {
            let done: Completion = decode(&op.payload, kind)?;
            closed.insert(done.prepared, kind);
            Left::Nothing
        }
        Some(&COMPLETE_CHECKOUT) => {
            let done: CheckoutDone = decode(&op.payload, COMPLETE_CHECKOUT)?;
            closed.insert(done.prepared, COMPLETE_CHECKOUT);
            Left::Nothing
        }
        Some(&PREPARE) => {
            let intent: IndexIntent = decode(&op.payload, PREPARE)?;
            if intent.workspace != *identity {
                return Ok(Left::Nothing);
            }
            // one that was abandoned or never closed left what was there
            let left = match closed.get(&id) {
                Some(&COMPLETE) => intent.after,
                _ => intent.before,
            };
            Left::Index(Some(left), false)
        }
        Some(&PREPARE_CHECKOUT) => {
            let intent: CheckoutIntent = decode(&op.payload, PREPARE_CHECKOUT)?;
            if intent.workspace != *identity {
                return Ok(Left::Nothing);
            }
            Left::Index(Some(checked_out(intent, closed.contains_key(&id))), false)
        }
        Some(&alt_refs::PAYLOAD_REF_TX) => {
            let Some(context) = alt_refs::parse_tx(&op.payload)?.and_then(|tx| tx.context) else {
                return Ok(Left::Nothing);
            };
            let context: RefContext = alt_json::from_slice(&context)?;
            if context.workspace != *identity {
                return Ok(Left::Nothing);
            }
            match (context.checkout, context.effects) {
                (Some(intent), _) => {
                    Left::Index(Some(checked_out(intent, closed.contains_key(&id))), true)
                }
                (None, RefEffects::RefsOnly) => Left::Refs,
                // nothing says what this one did to the index
                (None, _) => Left::Index(None, true),
            }
        }
        _ => Left::Nothing,
    })
}

fn checked_out(intent: CheckoutIntent, completed: bool) -> String {
    match completed {
        true => intent.index_after,
        false => intent.index_before,
    }
}
