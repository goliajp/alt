//! Whether a mark of a step came to hold.
//!
//! A mark that tells what holds with the next record holds once a record
//! of its own command comes after it in the log, a ref transaction or the
//! intent of a checkout that is not the guard, and not at all where
//! another mark of the workspace or the record that closes the step comes
//! first: what a later command writes is no record of the step. Bringing
//! a step to an end and telling the index a step left go by this alone.

use super::super::super::checkout_recovery::{CheckoutIntent, PREPARE_CHECKOUT};
use super::super::super::recovery::decode;
use super::super::super::*;
use super::mark::StepMark;
use super::{STEP, STEP_MARK};

impl NativeRepo<'_> {
    /// Whether the mark `op` came to hold, by the records `after` it.
    pub(in crate::native) fn mark_held(
        &self,
        op: &alt_oplog::Op,
        after: &[alt_oplog::Op],
    ) -> Res<bool> {
        let mark: StepMark = decode(&op.payload, STEP_MARK)?;
        if !mark.with_effect {
            return Ok(true);
        }
        for op in after {
            let kind = op.payload.first().copied();
            let told = [STEP, STEP_MARK, PREPARE_CHECKOUT, alt_refs::PAYLOAD_REF_TX];
            if !kind.is_some_and(|kind| told.contains(&kind)) {
                continue;
            }
            if self.operation_info(op)?.0.as_ref() != Some(&mark.workspace) {
                continue;
            }
            if matches!(kind, Some(STEP | STEP_MARK)) {
                return Ok(false);
            }
            if requests::operation_request(op)? != mark.request_id {
                continue;
            }
            if kind == Some(PREPARE_CHECKOUT) {
                let intent: CheckoutIntent = decode(&op.payload, PREPARE_CHECKOUT)?;
                if self.is_guard(&intent)? {
                    continue;
                }
            }
            return Ok(true);
        }
        Ok(false)
    }
}
