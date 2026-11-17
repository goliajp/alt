//! Record framing: encoding and parsing op bodies, and the replay walk that
//! verifies records and the hash chain between them.

use crate::{Op, OpId, OpLogError};

pub(crate) const MAGIC: [u8; 4] = *b"ALTL";
pub(crate) const VERSION: u8 = 1;
pub(crate) const HEADER_LEN: usize = 5;
/// parent 32 + change_id 32 + timestamp 8 + actor_len 2 + payload_len 4.
pub(crate) const BODY_FIXED_LEN: usize = 78;
pub(crate) const CHECKSUM_LEN: usize = 8;

pub(crate) fn file_header() -> [u8; HEADER_LEN] {
    [MAGIC[0], MAGIC[1], MAGIC[2], MAGIC[3], VERSION]
}

pub(crate) fn encode_body(
    parent: &OpId,
    timestamp_ms: u64,
    actor: &str,
    payload: &[u8],
) -> Vec<u8> {
    let mut body = Vec::with_capacity(BODY_FIXED_LEN + actor.len() + payload.len());
    body.extend_from_slice(&parent.0);
    body.extend_from_slice(&[0u8; 32]); // change_id, reserved
    body.extend_from_slice(&timestamp_ms.to_le_bytes());
    body.extend_from_slice(&(actor.len() as u16).to_le_bytes());
    body.extend_from_slice(actor.as_bytes());
    body.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    body.extend_from_slice(payload);
    body
}

/// Decodes a body whose BLAKE3 the caller already has as `id`.
pub(crate) fn parse_body(body: &[u8], id: OpId) -> Result<Op, OpLogError> {
    if body.len() < BODY_FIXED_LEN {
        return Err(OpLogError::Format("op body too short"));
    }
    let mut parent = [0u8; 32];
    parent.copy_from_slice(&body[..32]);
    let mut change_id = [0u8; 32];
    change_id.copy_from_slice(&body[32..64]);
    let timestamp_ms = u64::from_le_bytes(body[64..72].try_into().unwrap());
    let actor_len = u16::from_le_bytes(body[72..74].try_into().unwrap()) as usize;
    let rest = &body[74..];
    if rest.len() < actor_len + 4 {
        return Err(OpLogError::Format("op actor length mismatch"));
    }
    let actor = std::str::from_utf8(&rest[..actor_len])
        .map_err(|_| OpLogError::Format("op actor is not utf-8"))?
        .to_owned();
    let payload_len =
        u32::from_le_bytes(rest[actor_len..actor_len + 4].try_into().unwrap()) as usize;
    let payload = &rest[actor_len + 4..];
    if payload.len() != payload_len {
        return Err(OpLogError::Format("op payload length mismatch"));
    }
    Ok(Op {
        id,
        parent: OpId(parent),
        change_id,
        timestamp_ms,
        actor,
        payload: payload.to_vec(),
    })
}

/// A crash between create and the header fsync can leave a short file;
/// anything that is not a header prefix is foreign.
pub(crate) fn check_header(data: &[u8]) -> Result<(), OpLogError> {
    if data.len() < HEADER_LEN {
        if !file_header().starts_with(data) {
            return Err(OpLogError::Format("bad oplog header"));
        }
    } else {
        if data[..4] != MAGIC {
            return Err(OpLogError::Format("bad oplog header"));
        }
        if data[4] != VERSION {
            return Err(OpLogError::Format("unsupported oplog version"));
        }
    }
    Ok(())
}

pub(crate) struct Replayed {
    /// offset just past the last whole, valid record
    pub valid: usize,
    /// offset where that record starts, when there was one
    pub last_start: Option<usize>,
}

/// Whether no whole record can follow the one at `at`, where a walk
/// stopped short of the end of `data`: its length is cut short, or by its
/// own length it reaches the end or past it.
pub(crate) fn ends_the_log(data: &[u8], at: usize) -> bool {
    match data.get(at..at + 4) {
        Some(len) => {
            let body_len = u32::from_le_bytes(len.try_into().unwrap()) as usize;
            at.saturating_add(4 + body_len + CHECKSUM_LEN) >= data.len()
        }
        None => true,
    }
}

/// Walks length-prefixed records in `data[from..]`, verifying that the hash
/// chain continues from `prev_head`, pushing each op into `ops`. A record
/// that runs past the end or fails its checksum ends the walk: whether it
/// is an append that never finished or damage is for the caller to tell
/// from how far the log was synced, not from where in the file it sits.
pub(crate) fn replay_records(
    data: &[u8],
    from: usize,
    mut prev_head: OpId,
    ops: &mut Vec<Op>,
) -> Result<Replayed, OpLogError> {
    let mut at = from;
    let mut last_start = None;
    while let Some(len_bytes) = data.get(at..at + 4) {
        let body_len = u32::from_le_bytes(len_bytes.try_into().unwrap()) as usize;
        let Some(rec) = data.get(at + 4..at + 4 + body_len + CHECKSUM_LEN) else {
            break;
        };
        let (body, check) = rec.split_at(body_len);
        let hash = alt_hash::blake3::hash(body);
        if hash.as_bytes()[..CHECKSUM_LEN] != *check {
            break;
        }
        let op = parse_body(body, OpId(*hash.as_bytes()))?;
        if op.parent != prev_head {
            return Err(OpLogError::Format("oplog chain broken"));
        }
        prev_head = op.id;
        ops.push(op);
        last_start = Some(at);
        at += 4 + body_len + CHECKSUM_LEN;
    }
    Ok(Replayed {
        valid: at,
        last_start,
    })
}
