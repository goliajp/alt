//! Encoding for `PAYLOAD_INDEX_TX` (op kind 0x02): the change list for an
//! index-only mutation (today: `alt add`). One entry per touched path with
//! its old (None = was not in the index) and new (None = was removed)
//! side. `alt undo` parses this back and restores the index entries to
//! their prior state — A2's "any state-changing op is reversible" extended
//! beyond the ref-tx kind A4 was already covering.
//!
//! Wire format (little-endian, self-describing — the algo discriminator
//! travels in the payload so a future store could read its own log even
//! after migrating hash algos):
//!
//!   byte 0           PAYLOAD_INDEX_TX = 0x02
//!   byte 1           VERSION         = 1
//!   byte 2           algo            (0 = Sha1, 1 = Sha256)
//!   u32 LE           entry count
//!   per entry:
//!     u16 LE         path length
//!     N bytes        path
//!     u8             had_old (0 | 1)
//!     if had_old:
//!       raw oid bytes (algo's raw_len)
//!       u32 LE       mode
//!     u8             has_new (0 | 1)
//!     if has_new:
//!       raw oid bytes (algo's raw_len)
//!       u32 LE       mode

use alt_git_codec::{HashAlgo, ObjectId};
use bstr::BString;

/// Op kind discriminator for an index-only transaction. Distinct from
/// `alt_refs::PAYLOAD_REF_TX` (= 1) so undo can switch on the first byte.
pub const PAYLOAD_INDEX_TX: u8 = 2;

const VERSION: u8 = 1;
const ALGO_SHA1: u8 = 0;
const ALGO_SHA256: u8 = 1;

/// One path's transition during an index-touching op. The `oid + mode`
/// pair is enough to recreate the index entry on undo — the stat fields
/// get a zero baseline, which is exactly how git encodes an entry that
/// hasn't been stat-cached yet (the next status walk re-stamps them).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexChange {
    pub path: BString,
    pub old: Option<(ObjectId, u32)>,
    pub new: Option<(ObjectId, u32)>,
}

#[derive(Debug)]
pub enum IndexTxError {
    NotIndexTx,
    UnsupportedVersion(u8),
    UnknownAlgo(u8),
    Truncated,
    Oid(alt_git_codec::ParseOidError),
}

impl std::fmt::Display for IndexTxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotIndexTx => write!(f, "payload is not an index-tx (wrong kind byte)"),
            Self::UnsupportedVersion(v) => write!(f, "unsupported index-tx version {v}"),
            Self::UnknownAlgo(b) => write!(f, "unknown algo discriminator {b}"),
            Self::Truncated => write!(f, "index-tx payload truncated"),
            Self::Oid(e) => write!(f, "index-tx oid parse: {e}"),
        }
    }
}

impl std::error::Error for IndexTxError {}

impl From<alt_git_codec::ParseOidError> for IndexTxError {
    fn from(e: alt_git_codec::ParseOidError) -> Self {
        Self::Oid(e)
    }
}

fn algo_byte(algo: HashAlgo) -> u8 {
    match algo {
        HashAlgo::Sha1 => ALGO_SHA1,
        HashAlgo::Sha256 => ALGO_SHA256,
    }
}

fn algo_from_byte(b: u8) -> Result<HashAlgo, IndexTxError> {
    match b {
        ALGO_SHA1 => Ok(HashAlgo::Sha1),
        ALGO_SHA256 => Ok(HashAlgo::Sha256),
        other => Err(IndexTxError::UnknownAlgo(other)),
    }
}

/// Build the byte payload for a set of `IndexChange`s.
pub fn encode(changes: &[IndexChange], algo: HashAlgo) -> Vec<u8> {
    let mut out = Vec::with_capacity(3 + 4 + changes.len() * (2 + 32 + 1 + algo.raw_len() + 4 + 1));
    out.push(PAYLOAD_INDEX_TX);
    out.push(VERSION);
    out.push(algo_byte(algo));
    out.extend_from_slice(&(changes.len() as u32).to_le_bytes());
    for ch in changes {
        out.extend_from_slice(&(ch.path.len() as u16).to_le_bytes());
        out.extend_from_slice(&ch.path);
        match &ch.old {
            Some((oid, mode)) => {
                out.push(1);
                out.extend_from_slice(oid.as_bytes());
                out.extend_from_slice(&mode.to_le_bytes());
            }
            None => out.push(0),
        }
        match &ch.new {
            Some((oid, mode)) => {
                out.push(1);
                out.extend_from_slice(oid.as_bytes());
                out.extend_from_slice(&mode.to_le_bytes());
            }
            None => out.push(0),
        }
    }
    out
}

/// Parse an `index-tx` payload back into its change list.
pub fn decode(payload: &[u8]) -> Result<Vec<IndexChange>, IndexTxError> {
    if payload.first() != Some(&PAYLOAD_INDEX_TX) {
        return Err(IndexTxError::NotIndexTx);
    }
    let mut at = 1;
    if payload.len() < at + 1 {
        return Err(IndexTxError::Truncated);
    }
    let version = payload[at];
    at += 1;
    if version != VERSION {
        return Err(IndexTxError::UnsupportedVersion(version));
    }
    if payload.len() < at + 1 {
        return Err(IndexTxError::Truncated);
    }
    let algo = algo_from_byte(payload[at])?;
    at += 1;
    let oid_len = algo.raw_len();
    if payload.len() < at + 4 {
        return Err(IndexTxError::Truncated);
    }
    let count = u32::from_le_bytes(payload[at..at + 4].try_into().unwrap()) as usize;
    at += 4;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        if payload.len() < at + 2 {
            return Err(IndexTxError::Truncated);
        }
        let path_len = u16::from_le_bytes(payload[at..at + 2].try_into().unwrap()) as usize;
        at += 2;
        if payload.len() < at + path_len {
            return Err(IndexTxError::Truncated);
        }
        let path = BString::from(&payload[at..at + path_len]);
        at += path_len;

        let read_side = |at: &mut usize| -> Result<Option<(ObjectId, u32)>, IndexTxError> {
            if payload.len() < *at + 1 {
                return Err(IndexTxError::Truncated);
            }
            let flag = payload[*at];
            *at += 1;
            if flag == 0 {
                return Ok(None);
            }
            if payload.len() < *at + oid_len + 4 {
                return Err(IndexTxError::Truncated);
            }
            let oid = ObjectId::from_bytes(algo, &payload[*at..*at + oid_len])?;
            *at += oid_len;
            let mode = u32::from_le_bytes(payload[*at..*at + 4].try_into().unwrap());
            *at += 4;
            Ok(Some((oid, mode)))
        };
        let old = read_side(&mut at)?;
        let new = read_side(&mut at)?;
        out.push(IndexChange { path, old, new });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oid(b: u8) -> ObjectId {
        ObjectId::hash_object(HashAlgo::Sha1, alt_git_codec::ObjectKind::Blob, &[b])
    }

    #[test]
    fn round_trips_empty_change_list() {
        let payload = encode(&[], HashAlgo::Sha1);
        assert_eq!(payload[0], PAYLOAD_INDEX_TX);
        assert!(decode(&payload).unwrap().is_empty());
    }

    #[test]
    fn round_trips_added_modified_deleted() {
        let cases = vec![
            IndexChange {
                path: "newfile.txt".into(),
                old: None,
                new: Some((oid(1), 0o100644)),
            },
            IndexChange {
                path: "edited.rs".into(),
                old: Some((oid(2), 0o100644)),
                new: Some((oid(3), 0o100755)),
            },
            IndexChange {
                path: "removed.md".into(),
                old: Some((oid(4), 0o100644)),
                new: None,
            },
        ];
        let payload = encode(&cases, HashAlgo::Sha1);
        let back = decode(&payload).unwrap();
        assert_eq!(back, cases);
    }

    #[test]
    fn decode_rejects_wrong_kind() {
        assert!(matches!(decode(&[1, 0]), Err(IndexTxError::NotIndexTx)));
        assert!(matches!(decode(&[]), Err(IndexTxError::NotIndexTx)));
    }

    #[test]
    fn decode_rejects_unknown_version() {
        let payload = vec![PAYLOAD_INDEX_TX, 99, 0, 0, 0, 0, 0];
        assert!(matches!(
            decode(&payload),
            Err(IndexTxError::UnsupportedVersion(99))
        ));
    }

    #[test]
    fn decode_rejects_truncated_path() {
        let mut payload = encode(
            &[IndexChange {
                path: "x.txt".into(),
                old: None,
                new: Some((oid(1), 0o100644)),
            }],
            HashAlgo::Sha1,
        );
        // chop off the last byte of the new oid + mode
        payload.truncate(payload.len() - 5);
        assert!(matches!(decode(&payload), Err(IndexTxError::Truncated)));
    }
}
