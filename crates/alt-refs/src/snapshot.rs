//! Ref-state snapshot: pure replay acceleration, never truth. The snapshot
//! records the full ref map as of one op; opening starts there and replays
//! only later ops. A missing, corrupt, or orphaned (op no longer in the
//! log) snapshot is simply ignored and rebuilt — exactly like the pack idx,
//! the oplog stays the single source of state.
//!
//! Version 2 also records where that op's record sits in the log, so the
//! log itself is read from there on. Version 1 files (op id only) are still
//! read; they cost one full replay, after which a version 2 file replaces
//! them.

use std::fs::{self, File};
use std::io::Write;
use std::path::Path;

use alt_git_codec::{HashAlgo, ObjectId};
use alt_oplog::{Anchor, OpId};

use crate::state::Base;
use crate::{RefError, RefTarget};

const MAGIC: [u8; 4] = *b"ALTS";
const VERSION_OP_ONLY: u8 = 1;
const VERSION: u8 = 2;
/// start, end and count of the anchor, after the op id
const ANCHOR_LEN: usize = 24;

const TARGET_OID: u8 = 1;
const TARGET_SYMBOLIC: u8 = 2;

/// What a snapshot file holds.
pub struct Snapshot {
    pub at_op: OpId,
    /// where `at_op` sits in the log; absent in version 1 files
    pub anchor: Option<Anchor>,
    pub refs: Base,
    /// size of the file
    pub len: u64,
}

/// Writes the snapshot and returns its size. No fsync: a snapshot lost or
/// torn in a crash fails its checksum and is rebuilt from the log.
pub fn write<'a>(
    path: &Path,
    at: &Anchor,
    count: usize,
    refs: impl Iterator<Item = (&'a str, &'a RefTarget)>,
) -> Result<u64, RefError> {
    let mut buf = Vec::new();
    buf.extend_from_slice(&MAGIC);
    buf.push(VERSION);
    buf.extend_from_slice(&at.op.0);
    buf.extend_from_slice(&at.start.to_le_bytes());
    buf.extend_from_slice(&at.end.to_le_bytes());
    buf.extend_from_slice(&at.count.to_le_bytes());
    buf.extend_from_slice(&(count as u32).to_le_bytes());
    for (name, target) in refs {
        buf.extend_from_slice(&(name.len() as u16).to_le_bytes());
        buf.extend_from_slice(name.as_bytes());
        match target {
            RefTarget::Oid(oid) => {
                buf.push(TARGET_OID);
                buf.push(match oid.algo() {
                    HashAlgo::Sha1 => 1,
                    HashAlgo::Sha256 => 2,
                });
                buf.extend_from_slice(oid.as_bytes());
            }
            RefTarget::Symbolic(sym) => {
                buf.push(TARGET_SYMBOLIC);
                buf.extend_from_slice(&(sym.len() as u16).to_le_bytes());
                buf.extend_from_slice(sym.as_bytes());
            }
        }
    }
    let check: [u8; 8] = alt_hash::blake3::hash(&buf).as_bytes()[..8]
        .try_into()
        .unwrap();
    buf.extend_from_slice(&check);

    // readers refresh the snapshot too: one temp file per process
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    File::create(&tmp)?.write_all(&buf)?;
    fs::rename(&tmp, path)?;
    Ok(buf.len() as u64)
}

/// Reads a snapshot; any structural problem yields None (rebuild), never an
/// error — the snapshot is a cache.
pub fn read(path: &Path) -> Option<Snapshot> {
    let data = fs::read(path).ok()?;
    if data.len() < 5 + 32 + 4 + 8 || data[..4] != MAGIC {
        return None;
    }
    let anchored = match data[4] {
        VERSION => true,
        VERSION_OP_ONLY => false,
        _ => return None,
    };
    let (body, check) = data.split_at(data.len() - 8);
    if alt_hash::blake3::hash(body).as_bytes()[..8] != *check {
        return None;
    }

    let mut at_op = [0u8; 32];
    at_op.copy_from_slice(&body[5..37]);
    let at_op = OpId(at_op);
    let mut at = 37;
    let anchor = if anchored {
        let field = |i: usize| -> Option<u64> {
            Some(u64::from_le_bytes(
                body.get(37 + i * 8..45 + i * 8)?.try_into().ok()?,
            ))
        };
        at += ANCHOR_LEN;
        Some(Anchor {
            op: at_op,
            start: field(0)?,
            end: field(1)?,
            count: field(2)?,
        })
    } else {
        None
    };
    let count = u32::from_le_bytes(body.get(at..at + 4)?.try_into().ok()?) as usize;
    at += 4;
    let mut names: Vec<(u32, u16)> = Vec::with_capacity(count.min(body.len() / 4));
    let mut targets = Vec::with_capacity(count.min(body.len() / 4));
    let mut previous: Option<&str> = None;
    for _ in 0..count {
        let name_len = u16::from_le_bytes(body.get(at..at + 2)?.try_into().ok()?);
        at += 2;
        let name = std::str::from_utf8(body.get(at..at + name_len as usize)?).ok()?;
        // lookups search the names: they must ascend
        if previous.is_some_and(|previous| previous >= name) {
            return None;
        }
        previous = Some(name);
        names.push((u32::try_from(at).ok()?, name_len));
        at += name_len as usize;
        let target = match *body.get(at)? {
            TARGET_OID => {
                at += 1;
                let algo = match *body.get(at)? {
                    1 => HashAlgo::Sha1,
                    2 => HashAlgo::Sha256,
                    _ => return None,
                };
                at += 1;
                let oid = ObjectId::from_bytes(algo, body.get(at..at + algo.raw_len())?).ok()?;
                at += algo.raw_len();
                RefTarget::Oid(oid)
            }
            TARGET_SYMBOLIC => {
                at += 1;
                let len = u16::from_le_bytes(body.get(at..at + 2)?.try_into().ok()?) as usize;
                at += 2;
                let sym = std::str::from_utf8(body.get(at..at + len)?).ok()?;
                at += len;
                RefTarget::Symbolic(sym.to_owned())
            }
            _ => return None,
        };
        targets.push(target);
    }
    if at != body.len() {
        return None;
    }
    let len = data.len() as u64;
    let mut data = data;
    data.truncate(at);
    Some(Snapshot {
        at_op,
        anchor,
        refs: Base::new(data, names, targets),
        len,
    })
}
