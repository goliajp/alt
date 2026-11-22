//! Request scope and operation attribution shared by both execution paths.
use super::*;

impl Coord {
    pub fn request_workspace(&self) -> Res<String> {
        Ok(recovery::workspace_identity_at(&self.index_path, &self.workspace)?.instance)
    }
}

/// What the ops of the log say of requests, kept by a store that serves
/// more than one: the keys of ref transactions, and the operations each
/// request id is on. It follows the log as the log grows.
#[derive(Default)]
pub(super) struct Attributed {
    /// how many ops of the chain are taken in, and the last of them
    read: usize,
    last: Option<OpId>,
    keys: std::collections::HashSet<IdemKey>,
    operations: std::collections::HashMap<String, Vec<OpId>>,
}

impl Attributed {
    fn follow(&mut self, ops: &[alt_oplog::Op]) -> Res<()> {
        // an op's id names its whole history: another one where the last
        // op taken in was is another chain
        let last = self
            .read
            .checked_sub(1)
            .map(|at| ops.get(at).map(|op| op.id));
        if last.is_some_and(|id| id != self.last) {
            *self = Self::default();
        }
        for op in &ops[self.read..] {
            if let Some(tx) = alt_refs::parse_tx(&op.payload)?
                && let Some(key) = tx.key
            {
                self.keys.insert(key);
            }
            if let Some(request) = operation_request(op)? {
                self.operations.entry(request).or_default().push(op.id);
            }
            self.read += 1;
            self.last = Some(op.id);
        }
        Ok(())
    }
}

impl Store {
    fn attributed(&self) -> Res<std::sync::MutexGuard<'_, Attributed>> {
        let mut attributed = self.attributed.lock().expect("request index poisoned");
        attributed.follow(self.refs.oplog().ops()?)?;
        Ok(attributed)
    }

    pub fn legacy_request(&self, id: &IdemKey) -> Res<bool> {
        // The ref snapshot's key cache does not cover older history.
        Ok(self.attributed()?.keys.contains(id))
    }

    pub fn request_operations(&self, id: &str) -> Res<Vec<String>> {
        let attributed = self.attributed()?;
        let operations = attributed.operations.get(id);
        Ok(operations.map_or_else(Vec::new, |ops| ops.iter().map(OpId::to_string).collect()))
    }

    /// Where the log ends as this store holds it; `None` for an empty log.
    pub fn log_anchor(&self) -> Option<alt_oplog::Anchor> {
        self.refs.oplog().anchor()
    }

    /// [`request_operations`](Self::request_operations) of a request no
    /// operation carried before the log reached `began`
    /// ([`log_anchor`](Self::log_anchor)): only what was appended since is
    /// read. Without a `began` every op is.
    pub fn request_operations_since(
        &self,
        began: Option<Option<alt_oplog::Anchor>>,
        id: &str,
    ) -> Res<Vec<String>> {
        let log = self.refs.oplog();
        let since = match began {
            Some(Some(anchor)) => log.since(&anchor)?,
            Some(None) => Some(std::borrow::Cow::Borrowed(log.ops()?)),
            None => None,
        };
        // a log cut back past where the command began says nothing of
        // which ops are new
        let Some(since) = since else {
            return self.request_operations(id);
        };
        let mut operations = Vec::new();
        for op in since.iter() {
            if operation_request(op)?.as_deref() == Some(id) {
                operations.push(op.id.to_string());
            }
        }
        Ok(operations)
    }
}

pub(super) fn operation_request(op: &alt_oplog::Op) -> Res<Option<String>> {
    let context = match op.payload.first() {
        Some(&task::TASK_OPERATION)
        | Some(&recovery::PREPARE)
        | Some(&recovery::COMPLETE)
        | Some(&checkout_recovery::PREPARE_CHECKOUT)
        | Some(&checkout_recovery::COMPLETE_CHECKOUT)
        | Some(&in_progress::STEP)
        | Some(&in_progress::STEP_MARK) => {
            if op.payload.get(1) != Some(&1) {
                return Ok(None);
            }
            Some(op.payload[2..].to_vec())
        }
        Some(&alt_refs::PAYLOAD_REF_TX) => {
            let Some(tx) = alt_refs::parse_tx(&op.payload)? else {
                return Ok(None);
            };
            let key = tx.key.map(|key| crate::machine::hex(&key));
            if let Some(bytes) = tx.context {
                let value: alt_json::Value = alt_json::from_slice(&bytes)?;
                return Ok(value["request_id"].as_str().map(str::to_owned).or(key));
            }
            return Ok(key);
        }
        _ => None,
    };
    match context {
        Some(bytes) => Ok(
            alt_json::from_slice::<alt_json::Value>(&bytes)?["request_id"]
                .as_str()
                .map(str::to_owned),
        ),
        None => Ok(None),
    }
}

impl NativeRepo<'_> {
    pub fn request_query(
        &self,
        op: &crate::cli::RequestOp,
        json: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        crate::request::query(self.store, op, json, out)
    }
}
