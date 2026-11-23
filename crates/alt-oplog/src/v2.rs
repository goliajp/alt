//! Version 2 of the log file: frames in blocks, with checkpoints.

pub mod block;
pub mod checkpoint;
pub mod compose;
pub mod frame;
pub mod receipt;
pub mod scan;
pub mod state;

/// The body of an op that follows the op `parent`: what an op frame holds
/// and what the op's id is the hash of.
pub fn op_body(parent: &crate::OpId, timestamp_ms: u64, actor: &str, payload: &[u8]) -> Vec<u8> {
    crate::scan::encode_body(parent, timestamp_ms, actor, payload)
}
