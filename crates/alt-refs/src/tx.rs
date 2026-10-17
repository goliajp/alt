//! Ref-transaction payload codec: how a ref transaction is encoded into an
//! oplog op payload. The first payload byte tags the op kind so other op
//! kinds (imports, future workflow ops) share the same log.
//!
//! Each update carries the expected old target alongside the new one:
//! replay re-verifies the whole history deterministically, and undo
//! can be computed from the record alone.

use alt_git_codec::{HashAlgo, ObjectId};

use crate::{RefError, RefTarget};

/// Payload kind byte for ref transactions.
pub const PAYLOAD_REF_TX: u8 = 1;
/// v2 appends an optional idempotency key after the changes; v1 (no key) still
/// parses (key = None), so a store written before idempotency keys reads back unchanged.
const TX_VERSION: u8 = 2;

/// A client idempotency token, persisted in the ref transaction so a retried
/// write (even across a daemon restart) is detected and not applied twice.
pub type IdemKey = [u8; 16];

/// A parsed ref transaction: its changes and the optional idempotency key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedTx {
    pub changes: Vec<RefChange>,
    pub key: Option<IdemKey>,
    /// Caller-owned recovery context. Absent in legacy transactions.
    pub context: Option<Vec<u8>>,
}

const TARGET_ABSENT: u8 = 0;
const TARGET_OID: u8 = 1;
const TARGET_SYMBOLIC: u8 = 2;

/// One ref change inside a transaction: `old` is what the ref must point
/// at when the transaction applies (None = must be absent), `new` is what
/// it points at afterwards (None = delete).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefChange {
    pub name: String,
    pub old: Option<RefTarget>,
    pub new: Option<RefTarget>,
}

fn encode_target(out: &mut Vec<u8>, target: &Option<RefTarget>) {
    match target {
        None => out.push(TARGET_ABSENT),
        Some(RefTarget::Oid(oid)) => {
            out.push(TARGET_OID);
            out.push(match oid.algo() {
                HashAlgo::Sha1 => 1,
                HashAlgo::Sha256 => 2,
            });
            out.extend_from_slice(oid.as_bytes());
        }
        Some(RefTarget::Symbolic(name)) => {
            out.push(TARGET_SYMBOLIC);
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(name.as_bytes());
        }
    }
}

fn parse_target(data: &[u8], at: &mut usize) -> Result<Option<RefTarget>, RefError> {
    let tag = *data.get(*at).ok_or(RefError::Format("ref tx truncated"))?;
    *at += 1;
    Ok(match tag {
        TARGET_ABSENT => None,
        TARGET_OID => {
            let algo = match data.get(*at) {
                Some(1) => HashAlgo::Sha1,
                Some(2) => HashAlgo::Sha256,
                _ => return Err(RefError::Format("bad hash algo in ref tx")),
            };
            *at += 1;
            let raw = data
                .get(*at..*at + algo.raw_len())
                .ok_or(RefError::Format("ref tx truncated"))?;
            *at += algo.raw_len();
            let oid = ObjectId::from_bytes(algo, raw)
                .map_err(|_| RefError::Format("bad oid in ref tx"))?;
            Some(RefTarget::Oid(oid))
        }
        TARGET_SYMBOLIC => {
            let len_bytes = data
                .get(*at..*at + 2)
                .ok_or(RefError::Format("ref tx truncated"))?;
            let len = u16::from_le_bytes(len_bytes.try_into().unwrap()) as usize;
            *at += 2;
            let raw = data
                .get(*at..*at + len)
                .ok_or(RefError::Format("ref tx truncated"))?;
            *at += len;
            let name = std::str::from_utf8(raw)
                .map_err(|_| RefError::Format("symref target is not utf-8"))?;
            Some(RefTarget::Symbolic(name.to_owned()))
        }
        _ => return Err(RefError::Format("bad target tag in ref tx")),
    })
}

pub fn encode_tx(changes: &[RefChange], key: Option<IdemKey>) -> Vec<u8> {
    let mut out = vec![PAYLOAD_REF_TX, TX_VERSION];
    out.extend_from_slice(&(changes.len() as u32).to_le_bytes());
    for change in changes {
        out.extend_from_slice(&(change.name.len() as u16).to_le_bytes());
        out.extend_from_slice(change.name.as_bytes());
        encode_target(&mut out, &change.old);
        encode_target(&mut out, &change.new);
    }
    // optional idempotency key: a length byte (0 or 16) then the key bytes
    match key {
        Some(k) => {
            out.push(k.len() as u8);
            out.extend_from_slice(&k);
        }
        None => out.push(0),
    }
    out
}

/// Parses a ref-transaction payload; returns None for other op kinds.
pub fn parse_tx(payload: &[u8]) -> Result<Option<ParsedTx>, RefError> {
    if payload.first() != Some(&PAYLOAD_REF_TX) {
        return Ok(None);
    }
    let version = payload.get(1).copied();
    if !matches!(version, Some(1..=3)) {
        return Err(RefError::Format("unsupported ref tx version"));
    }
    let count_bytes = payload
        .get(2..6)
        .ok_or(RefError::Format("ref tx truncated"))?;
    let count = u32::from_le_bytes(count_bytes.try_into().unwrap()) as usize;
    let mut at = 6;
    if count > payload.len() / 4 {
        return Err(RefError::Format("ref tx count exceeds payload"));
    }
    let mut changes = Vec::with_capacity(count);
    for _ in 0..count {
        let len_bytes = payload
            .get(at..at + 2)
            .ok_or(RefError::Format("ref tx truncated"))?;
        let name_len = u16::from_le_bytes(len_bytes.try_into().unwrap()) as usize;
        at += 2;
        let raw = payload
            .get(at..at + name_len)
            .ok_or(RefError::Format("ref tx truncated"))?;
        at += name_len;
        let name = std::str::from_utf8(raw)
            .map_err(|_| RefError::Format("ref name is not utf-8"))?
            .to_owned();
        let old = parse_target(payload, &mut at)?;
        let new = parse_target(payload, &mut at)?;
        changes.push(RefChange { name, old, new });
    }
    // v1 ends here (no key field); v2 carries the optional key
    let key = if version == Some(1) {
        None
    } else {
        let key_len = *payload
            .get(at)
            .ok_or(RefError::Format("ref tx truncated"))? as usize;
        at += 1;
        match key_len {
            0 => None,
            16 => {
                let raw = payload
                    .get(at..at + 16)
                    .ok_or(RefError::Format("ref tx truncated"))?;
                at += 16;
                Some(raw.try_into().unwrap())
            }
            _ => return Err(RefError::Format("bad idempotency key length in ref tx")),
        }
    };
    let context = if version == Some(3) {
        let len = payload
            .get(at..at + 4)
            .ok_or(RefError::Format("ref context truncated"))?;
        let len = u32::from_le_bytes(len.try_into().unwrap()) as usize;
        at += 4;
        let bytes = payload
            .get(at..at + len)
            .ok_or(RefError::Format("ref context truncated"))?;
        at += len;
        Some(bytes.to_vec())
    } else {
        None
    };
    if at != payload.len() {
        return Err(RefError::Format("ref tx has trailing bytes"));
    }
    Ok(Some(ParsedTx {
        changes,
        key,
        context,
    }))
}

/// Version 3 preserves an opaque recovery context in the same atomic ref op.
pub fn encode_scoped_tx(changes: &[RefChange], key: Option<IdemKey>, context: &[u8]) -> Vec<u8> {
    let mut out = encode_tx(changes, key);
    out[1] = 3;
    out.extend_from_slice(&(context.len() as u32).to_le_bytes());
    out.extend_from_slice(context);
    out
}

#[cfg(test)]
mod scoped_tests {
    use super::*;

    #[test]
    fn context_round_trip_and_legacy_remain_distinct() {
        let changes = vec![RefChange {
            name: "HEAD".to_owned(),
            old: None,
            new: Some(RefTarget::Symbolic("refs/heads/main".to_owned())),
        }];
        let scoped = parse_tx(&encode_scoped_tx(&changes, Some([7; 16]), b"scope"))
            .unwrap()
            .unwrap();
        assert_eq!(scoped.changes, changes);
        assert_eq!(scoped.key, Some([7; 16]));
        assert_eq!(scoped.context.as_deref(), Some(b"scope".as_slice()));
        let mut legacy = encode_tx(&changes, None);
        assert_eq!(parse_tx(&legacy).unwrap().unwrap().context, None);
        legacy[1] = 1;
        legacy.pop();
        assert_eq!(parse_tx(&legacy).unwrap().unwrap().context, None);
    }

    #[test]
    fn truncated_context_and_unbounded_counts_are_rejected() {
        let mut bytes = encode_scoped_tx(&[], None, b"scope");
        bytes.pop();
        assert!(parse_tx(&bytes).is_err());
        assert!(parse_tx(&[1, 3, 255, 255, 255, 255]).is_err());
    }
}
