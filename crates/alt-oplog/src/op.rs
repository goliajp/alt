//! The op record and its identity, and the log's error type.

use std::fmt;

#[derive(Debug)]
pub enum OpLogError {
    Io(std::io::Error),
    Format(&'static str),
    TooLarge(usize),
    /// the log holds whole records only up to byte `holds`, short of the
    /// byte it was synced to
    Lost {
        synced: u64,
        holds: u64,
    },
    /// the log was cut back past records this handle read or wrote
    CutBack,
    /// The op at byte `unheld` past where the log is known synced names
    /// content that is not stored, and a frame after it, at byte `kept`,
    /// was left by a writer that syncs, with what it names stored. A sync
    /// may have covered that frame without seeing to the op at `unheld`,
    /// and a command may have been answered for it. The log is not cut.
    Kept {
        unheld: u64,
        kept: u64,
    },
}

impl std::fmt::Display for OpLogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(_) => f.write_str("io"),
            Self::Format(v) => write!(f, "oplog format: {v}"),
            Self::TooLarge(v) => write!(f, "op too large: {v} bytes"),
            Self::Lost { synced, holds } => write!(
                f,
                "the operation log ends at byte {holds}, but records up to byte {synced} of \
                 it were committed and synced: committed operations are gone"
            ),
            Self::Kept { unheld, kept } => write!(
                f,
                "the operation at byte {unheld} of the operation log names content that is \
                 not stored, and an operation to keep follows it at byte {kept} in the part \
                 of the log that is not known to be synced: the log is left as it is"
            ),
            Self::CutBack => f.write_str(
                "the operation log was cut back: operations that were never synced and \
                 name objects that are not stored were discarded",
            ),
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
    /// Where a version 2 log holds the op; `None` for an op of a version
    /// 1 log and for one made of a body alone.
    pub frame: Option<Framed>,
}

/// The frame of an op in a version 2 log: where it lies and what it says
/// of the op beside its body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Framed {
    pub at: u64,
    pub end: u64,
    /// the op's number in the log, counted from one
    pub seq: u64,
    pub flags: u8,
    /// zeros for an op of no workspace
    pub workspace: [u8; 8],
    /// a ref transaction: its frame names the one before it
    pub linked: bool,
    /// the frame of the op this one undoes
    pub inverse_of: Option<u64>,
    pub request: Option<[u8; 16]>,
    /// the frame says what closes the index of its workspace once the op
    /// is done: the op changes that index
    pub staged: bool,
    /// the workspace is gone with this op
    pub retired: bool,
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
