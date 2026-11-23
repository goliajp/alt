//! Ops in the shared log that are no ref transactions.

use super::*;

impl RefStore {
    /// Records a non-ref op (import, future workflow ops) in the shared
    /// log. The ref-transaction kind byte is reserved.
    pub fn record_op(
        &mut self,
        actor: &str,
        timestamp_ms: u64,
        payload: &[u8],
    ) -> Result<OpId, RefError> {
        let op_id = self.record_op_unsynced(actor, timestamp_ms, payload)?;
        self.oplog.sync()?;
        Ok(op_id)
    }

    /// [`record_op`](Self::record_op) without its sync: the op is durable
    /// once a later one is, and a crash before that leaves it out. For an
    /// op that only says what the ops after it belong to.
    pub fn record_op_unsynced(
        &mut self,
        actor: &str,
        timestamp_ms: u64,
        payload: &[u8],
    ) -> Result<OpId, RefError> {
        if payload.first() == Some(&PAYLOAD_REF_TX) {
            return Err(RefError::ReservedPayload(PAYLOAD_REF_TX));
        }
        let op_id = self.append_op(actor, timestamp_ms, false, |_| Ok(payload.to_vec()))?;
        self.applied = self.oplog.recent().len();
        self.note_op()?;
        Ok(op_id)
    }
}
