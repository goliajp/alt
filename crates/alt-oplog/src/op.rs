//! The op record and its identity, and the log's error type.

use std::fmt;

#[derive(Debug)]
pub enum OpLogError {
    Io(std::io::Error),
    Format(&'static str),
    TooLarge(usize),
}

impl std::fmt::Display for OpLogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(_) => f.write_str("io"),
            Self::Format(v) => write!(f, "oplog format: {v}"),
            Self::TooLarge(v) => write!(f, "op too large: {v} bytes"),
        }
    }
}

impl std::error::Error for OpLogError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for OpLogError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// BLAKE3 of an op's body — its identity in the chain.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OpId(pub [u8; 32]);

/// The parent named by the first op in a log.
pub const ROOT: OpId = OpId([0u8; 32]);

impl fmt::Display for OpId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for OpId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "OpId({self})")
    }
}

/// One recorded operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Op {
    pub id: OpId,
    pub parent: OpId,
    /// Reserved for the change-identity model; zeros until then.
    pub change_id: [u8; 32],
    pub timestamp_ms: u64,
    /// Who performed the op — human or agent identity string.
    pub actor: String,
    /// Opaque to the log; the caller's encoding of what happened.
    pub payload: Vec<u8>,
}

impl Op {
    /// Decode an original operation body, preserving its content identity.
    /// This validates framing, not the authority or meaning of its payload.
    pub fn from_body(body: &[u8]) -> Result<Self, OpLogError> {
        crate::scan::parse_body(body, OpId(*alt_hash::blake3::hash(body).as_bytes()))
    }

    /// Return the exact canonical body of this operation for a scoped source
    /// proof. Reject edited fields rather than silently changing its identity.
    pub fn body(&self) -> Result<Vec<u8>, OpLogError> {
        if self.actor.len() > u16::MAX as usize || self.payload.len() > u32::MAX as usize {
            return Err(OpLogError::Format("operation field exceeds encoding limit"));
        }
        let mut body =
            crate::scan::encode_body(&self.parent, self.timestamp_ms, &self.actor, &self.payload);
        body[32..64].copy_from_slice(&self.change_id);
        if alt_hash::blake3::hash(&body).as_bytes() != &self.id.0 {
            return Err(OpLogError::Format(
                "operation body differs from its identity",
            ));
        }
        Ok(body)
    }
}
