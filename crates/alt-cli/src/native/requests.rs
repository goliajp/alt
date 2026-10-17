//! Request scope and operation attribution shared by both execution paths.
use super::*;

impl Coord {
    pub fn request_workspace(&self) -> Res<String> {
        Ok(recovery::workspace_identity_at(&self.index_path, &self.workspace)?.instance)
    }
}

impl Store {
    pub fn legacy_request(&self, id: &IdemKey) -> Res<bool> {
        // The ref snapshot's key cache does not cover older history.
        for op in self.refs.oplog().ops() {
            if let Some(tx) = alt_refs::parse_tx(&op.payload)?
                && tx.key.as_ref() == Some(id)
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub fn request_operations(&self, id: &str) -> Res<Vec<String>> {
        let mut operations = Vec::new();
        for op in self.refs.oplog().ops() {
            if operation_request(op)?.as_deref() == Some(id) {
                operations.push(op.id.to_string());
            }
        }
        Ok(operations)
    }
}

pub(super) fn operation_request(op: &alt_oplog::Op) -> Res<Option<String>> {
    let context = match op.payload.first() {
        Some(&recovery::PREPARE)
        | Some(&recovery::COMPLETE)
        | Some(&checkout_recovery::PREPARE_CHECKOUT)
        | Some(&checkout_recovery::COMPLETE_CHECKOUT) => {
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
                let value: serde_json::Value = serde_json::from_slice(&bytes)?;
                return Ok(value["request_id"].as_str().map(str::to_owned).or(key));
            }
            return Ok(key);
        }
        _ => None,
    };
    match context {
        Some(bytes) => Ok(
            serde_json::from_slice::<serde_json::Value>(&bytes)?["request_id"]
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
