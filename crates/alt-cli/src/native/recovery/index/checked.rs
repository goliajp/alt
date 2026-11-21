//! How far the operation log has been read for what a workspace prepared
//! and nothing closed. The log only grows, so an operation that was closed
//! stays closed: once every operation of the workspace up to an op is
//! known to be closed, a later check reads what follows that op and no
//! more. The place is kept beside the workspace's index as an anchor of
//! the log. It is a cache: a file that is missing, torn, of another
//! workspace or of a chain the log no longer holds is not used, and the
//! whole log is read as if it were not there.

use std::borrow::Cow;
use std::path::Path;

use alt_oplog::{Anchor, Op, OpId, OpLog};

use super::WorkspaceIdentity;
use crate::native::Res;

pub(super) const NAME: &str = "recovery.checked";

const MAGIC: [u8; 8] = *b"ALTRCHK\x01";
const SUM: usize = 8;
/// magic, the workspace, the op, where its record starts and ends, its count
const LEN: usize = 8 + 32 + 32 + 3 * 8 + SUM;

fn workspace(identity: &WorkspaceIdentity) -> [u8; 32] {
    let named = [
        identity.name.as_bytes(),
        b"\0",
        identity.instance.as_bytes(),
    ]
    .concat();
    *alt_hash::blake3::hash(&named).as_bytes()
}

/// The record that the workspace `identity` has nothing open up to
/// `anchor`.
pub(super) fn encode(identity: &WorkspaceIdentity, anchor: &Anchor) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(LEN);
    bytes.extend_from_slice(&MAGIC);
    bytes.extend_from_slice(&workspace(identity));
    bytes.extend_from_slice(&anchor.op.0);
    for number in [anchor.start, anchor.end, anchor.count] {
        bytes.extend_from_slice(&number.to_le_bytes());
    }
    let sum = alt_hash::blake3::hash(&bytes);
    bytes.extend_from_slice(&sum.as_bytes()[..SUM]);
    bytes
}

fn decode(bytes: &[u8], identity: &WorkspaceIdentity) -> Option<Anchor> {
    let (body, sum) = bytes.get(..LEN)?.split_at(LEN - SUM);
    if sum != &alt_hash::blake3::hash(body).as_bytes()[..SUM]
        || body[..8] != MAGIC
        || body[8..40] != workspace(identity)
    {
        return None;
    }
    let number = |at: usize| u64::from_le_bytes(body[at..at + 8].try_into().unwrap());
    Some(Anchor {
        op: OpId(body[40..72].try_into().unwrap()),
        start: number(72),
        end: number(80),
        count: number(88),
    })
}

/// Keeps `record` ([`encode`]) at `place`. One write of the whole record,
/// in place: a reader that meets half of it finds a sum that does not
/// match.
pub(super) fn keep(place: &Path, record: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::FileExt;
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(place)?;
    file.write_all_at(record, 0)
}

/// The ops of `log` the workspace `identity` has not been checked
/// against: those after the anchor kept at `place`, or all of them.
pub(super) fn unread<'a>(
    log: &'a OpLog,
    place: &Path,
    identity: &WorkspaceIdentity,
) -> Res<Cow<'a, [Op]>> {
    let kept = std::fs::read(place).ok();
    if let Some(anchor) = kept.and_then(|bytes| decode(&bytes, identity))
        && let Some(ops) = log.since(&anchor)?
    {
        return Ok(ops);
    }
    Ok(Cow::Borrowed(log.ops()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(name: &str) -> WorkspaceIdentity {
        WorkspaceIdentity {
            name: name.into(),
            instance: "0".repeat(64),
        }
    }

    fn ops_of(ops: &[Op]) -> Vec<Vec<u8>> {
        ops.iter().map(|op| op.payload.clone()).collect()
    }

    #[test]
    fn what_was_kept_shortens_the_read_only_while_it_holds() {
        let dir = alt_os::temp::tempdir().unwrap();
        let place = dir.path().join(NAME);
        let mut log = OpLog::open(&dir.path().join("oplog")).unwrap();
        log.append("a", 1, b"one").unwrap();
        log.append("a", 2, b"two").unwrap();
        let ours = identity("default");
        let all = |log: &OpLog| ops_of(log.ops().unwrap());
        // nothing kept: everything
        assert_eq!(ops_of(&unread(&log, &place, &ours).unwrap()), all(&log));
        keep(&place, &encode(&ours, &log.anchor().unwrap())).unwrap();
        assert!(unread(&log, &place, &ours).unwrap().is_empty());
        log.append("a", 3, b"three").unwrap();
        assert_eq!(
            ops_of(&unread(&log, &place, &ours).unwrap()),
            [b"three".to_vec()]
        );
        // another workspace's file says nothing of this one
        assert_eq!(
            ops_of(&unread(&log, &place, &identity("side")).unwrap()),
            all(&log)
        );
        // every torn or altered form of the file reads as no file
        let whole = std::fs::read(&place).unwrap();
        assert_eq!(whole.len(), LEN);
        for cut in 0..LEN {
            std::fs::write(&place, &whole[..cut]).unwrap();
            assert_eq!(
                ops_of(&unread(&log, &place, &ours).unwrap()),
                all(&log),
                "cut at {cut}"
            );
            let mut flipped = whole.clone();
            flipped[cut] ^= 1;
            std::fs::write(&place, &flipped).unwrap();
            assert_eq!(
                ops_of(&unread(&log, &place, &ours).unwrap()),
                all(&log),
                "bit at {cut}"
            );
        }
        // a longer file left by an earlier, larger record: the head of it counts
        std::fs::write(&place, [whole.clone(), vec![7; 9]].concat()).unwrap();
        assert_eq!(
            ops_of(&unread(&log, &place, &ours).unwrap()),
            [b"three".to_vec()]
        );
    }

    #[test]
    fn an_anchor_of_a_chain_the_log_no_longer_holds_is_not_used() {
        let dir = alt_os::temp::tempdir().unwrap();
        let place = dir.path().join(NAME);
        let ours = identity("default");
        let other = alt_os::temp::tempdir().unwrap();
        let mut elsewhere = OpLog::open(other.path()).unwrap();
        elsewhere.append("b", 1, b"other").unwrap();
        keep(&place, &encode(&ours, &elsewhere.anchor().unwrap())).unwrap();
        let mut log = OpLog::open(&dir.path().join("oplog")).unwrap();
        log.append("a", 1, b"one").unwrap();
        log.append("a", 2, b"two").unwrap();
        assert_eq!(unread(&log, &place, &ours).unwrap().len(), 2);
    }
}
