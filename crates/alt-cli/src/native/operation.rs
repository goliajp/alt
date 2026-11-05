//! Inspect durable local operation state and explicitly finish interrupted
//! writes. Recovery never guesses ownership and never overwrites a third state.

use super::*;
use crate::json::Json;
use checkout_recovery::{COMPLETE_CHECKOUT, CheckoutIntent, PREPARE_CHECKOUT};
use recovery::{COMPLETE, IndexIntent, PREPARE, RefContext, WorkspaceIdentity, decode};

impl NativeRepo<'_> {
    fn operation_by_id(&self, id: &str) -> Res<alt_oplog::Op> {
        if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(crate::machine::Failure::usage(
                "operation id must be 64 hexadecimal digits",
            )
            .into());
        }
        self.store
            .refs
            .oplog()
            .ops()
            .iter()
            .find(|op| op.id.to_string() == id)
            .cloned()
            .ok_or_else(|| "operation not found".into())
    }

    fn operation_complete(&self, id: OpId, kind: u8) -> Res<bool> {
        for op in self.store.refs.oplog().ops().iter().rev() {
            if op.payload.first() != Some(&kind) {
                continue;
            }
            let done: serde_json::Value = decode(&op.payload, kind)?;
            if done["version"] != 1 {
                return Err(crate::machine::Failure::refusal(
                    "unsupported_record",
                    "unsupported operation completion",
                )
                .into());
            }
            if done["prepared"].as_str() == Some(&id.to_string()) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn operation_info(&self, op: &alt_oplog::Op) -> Res<(Option<WorkspaceIdentity>, bool, bool)> {
        match op.payload.first() {
            Some(&task::TASK_OPERATION) => {
                let operation: task::TaskOperation = decode(&op.payload, task::TASK_OPERATION)?;
                if operation.version != 1 {
                    return Err(crate::machine::Failure::refusal(
                        "unsupported_record",
                        "unsupported task operation version",
                    )
                    .into());
                }
                Ok((Some(operation.workspace), true, false))
            }
            Some(&PREPARE) => {
                let intent: IndexIntent = decode(&op.payload, PREPARE)?;
                Ok((
                    Some(intent.workspace),
                    self.operation_complete(op.id, COMPLETE)?,
                    true,
                ))
            }
            Some(&PREPARE_CHECKOUT) => {
                let intent: CheckoutIntent = decode(&op.payload, PREPARE_CHECKOUT)?;
                Ok((
                    Some(intent.workspace),
                    self.operation_complete(op.id, COMPLETE_CHECKOUT)?,
                    true,
                ))
            }
            Some(&alt_refs::PAYLOAD_REF_TX) => {
                let tx = alt_refs::parse_tx(&op.payload)?.ok_or("not a ref operation")?;
                match tx.context {
                    Some(bytes) => {
                        let context: RefContext = serde_json::from_slice(&bytes)?;
                        if !matches!(context.version, 1 | 2) {
                            return Err(crate::machine::Failure::refusal(
                                "unsupported_record",
                                "unsupported ref context",
                            )
                            .into());
                        }
                        let complete = context.checkout.is_none()
                            || self.operation_complete(op.id, COMPLETE_CHECKOUT)?;
                        Ok((
                            Some(context.workspace),
                            complete,
                            context.checkout.is_some(),
                        ))
                    }
                    None => Ok((None, true, false)),
                }
            }
            _ => Ok((None, true, false)),
        }
    }

    fn operation_json(&self, op: &alt_oplog::Op) -> Res<Json> {
        let (workspace, complete, recoverable) = self.operation_info(op)?;
        let scoped = workspace.is_some();
        let task = if op.payload.first() == Some(&task::TASK_OPERATION) {
            Some(decode::<task::TaskOperation>(
                &op.payload,
                task::TASK_OPERATION,
            )?)
        } else {
            None
        };
        Ok(Json::Object(vec![
            ("operation_id", Json::str(op.id.to_string())),
            (
                "task_id",
                task.as_ref()
                    .map(|task| Json::str(&task.task_id))
                    .unwrap_or(Json::Null),
            ),
            (
                "record_id",
                task.as_ref()
                    .map(|task| Json::str(&task.record_id))
                    .unwrap_or(Json::Null),
            ),
            (
                "action",
                task.as_ref()
                    .map(|task| Json::str(&task.action))
                    .unwrap_or(Json::Null),
            ),
            (
                "request_id",
                requests::operation_request(op)?
                    .map(Json::str)
                    .unwrap_or(Json::Null),
            ),
            (
                "state",
                Json::str(if !scoped {
                    "legacy_or_unsupported"
                } else if complete {
                    "completed"
                } else {
                    "prepared"
                }),
            ),
            (
                "outcome",
                Json::str(if complete && scoped {
                    "applied"
                } else {
                    "unknown"
                }),
            ),
            ("recoverable", Json::Bool(recoverable)),
            (
                "workspace",
                match workspace {
                    Some(w) => Json::Object(vec![
                        ("name", Json::str(w.name)),
                        ("instance", Json::str(w.instance)),
                    ]),
                    None => Json::Null,
                },
            ),
        ]))
    }

    pub fn operation_pending(&self, json: bool, out: &mut impl Write) -> Res<()> {
        let instance = match std::fs::read_to_string(self.index_path.with_file_name("workspace-id"))
        {
            Ok(id) => Some(id),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        let mut ops = Vec::new();
        for op in self.store.refs.oplog().ops() {
            let (workspace, completed, _) = self.operation_info(op)?;
            if !completed
                && workspace
                    .is_some_and(|w| w.name == self.workspace && Some(w.instance) == instance)
            {
                if json {
                    ops.push(self.operation_json(op)?);
                } else {
                    writeln!(out, "{} prepared (outcome unknown)", op.id)?;
                }
            }
        }
        if json {
            crate::json::emit(out, vec![("operations", Json::Array(ops))])?;
        }
        Ok(())
    }

    pub fn operation_status(&self, id: &str, json: bool, out: &mut impl Write) -> Res<()> {
        let op = self.operation_by_id(id)?;
        if json {
            crate::json::emit(out, vec![("operation", self.operation_json(&op)?)])?;
        } else {
            let (_, complete, _) = self.operation_info(&op)?;
            writeln!(
                out,
                "{} {}",
                op.id,
                if complete {
                    "completed"
                } else {
                    "prepared (outcome unknown)"
                }
            )?;
        }
        Ok(())
    }

    pub fn operation_recover(&mut self, id: &str, json: bool, out: &mut impl Write) -> Res<()> {
        // Pending state is the reason for this command; unlike ordinary writes
        // it checks capabilities and takes the lock without rejecting pending.
        if self.caps.read_only {
            return Err(crate::machine::Failure::refusal(
                "capability_denied",
                "capability denied: principal cannot recover (read-only)",
            )
            .into());
        }
        self.lock_workspace()?;
        if self.caps.read_only {
            return Err(crate::machine::Failure::refusal(
                "capability_denied",
                "capability denied: principal cannot recover (read-only)",
            )
            .into());
        }
        let op = self.operation_by_id(id)?;
        let (workspace, completed, recoverable) = self.operation_info(&op)?;
        if workspace.as_ref() != Some(&self.workspace_identity()?) {
            return Err(crate::machine::Failure::refusal(
                "precondition_failed",
                "operation lacks matching workspace instance ownership",
            )
            .into());
        }
        if !recoverable {
            return Err(
                "operation has no prepared local recovery; use undo for a completed operation"
                    .into(),
            );
        }
        if !completed {
            match op.payload.first() {
                Some(&PREPARE) => self.recover_index(op.id, decode(&op.payload, PREPARE)?)?,
                Some(&PREPARE_CHECKOUT) => {
                    self.finish_interrupted_checkout(op.id, decode(&op.payload, PREPARE_CHECKOUT)?)?
                }
                Some(&alt_refs::PAYLOAD_REF_TX) => {
                    let tx = alt_refs::parse_tx(&op.payload)?.ok_or("not a ref operation")?;
                    for change in &tx.changes {
                        if self.store.refs.get(&change.name) != change.new.as_ref() {
                            return Err(crate::machine::Failure::refusal(
                                "precondition_failed",
                                "recovery precondition failed: refs changed",
                            )
                            .into());
                        }
                    }
                    let context: RefContext =
                        serde_json::from_slice(&tx.context.ok_or("missing recovery scope")?)?;
                    self.finish_interrupted_checkout(
                        op.id,
                        context.checkout.ok_or("missing checkout intent")?,
                    )?;
                }
                _ => {
                    return Err(crate::machine::Failure::refusal(
                        "unsupported_record",
                        "unsupported recovery operation",
                    )
                    .into());
                }
            }
        }
        self.operation_status(id, json, out)
    }
}
