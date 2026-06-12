//! Ref-state snapshot: pure replay acceleration, never truth. The snapshot
//! records the full ref map as of one op id; opening starts there and
//! replays only later ops. A missing, corrupt, or orphaned (op id no longer
//! in the log) snapshot is simply ignored and rebuilt — exactly like the
//! pack idx, the oplog stays the single source of state.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::Path;

use alt_git_codec::{HashAlgo, ObjectId};
use alt_oplog::OpId;

use crate::{RefError, RefTarget};

const MAGIC: [u8; 4] = *b"ALTS";
const VERSION: u8 = 1;

const TARGET_OID: u8 = 1;
const TARGET_SYMBOLIC: u8 = 2;

pub fn write(
    path: &Path,
    at_op: &OpId,
    refs: &BTreeMap<String, RefTarget>,
) -> Result<(), RefError> {
    let mut buf = Vec::new();
    buf.extend_from_slice(&MAGIC);
    buf.push(VERSION);
    buf.extend_from_slice(&at_op.0);
    buf.extend_from_slice(&(refs.len() as u32).to_le_bytes());
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
    let check: [u8; 8] = blake3::hash(&buf).as_bytes()[..8].try_into().unwrap();
    buf.extend_from_slice(&check);

    let tmp = path.with_extension("tmp");
    let mut file = File::create(&tmp)?;
    file.write_all(&buf)?;
    file.sync_all()?;
    fs::rename(&tmp, path)?;
    Ok(())
}

/// Reads a snapshot; any structural problem yields None (rebuild), never an
/// error — the snapshot is a cache.
pub fn read(path: &Path) -> Option<(OpId, BTreeMap<String, RefTarget>)> {
    let data = fs::read(path).ok()?;
    if data.len() < 5 + 32 + 4 + 8 || data[..4] != MAGIC || data[4] != VERSION {
        return None;
    }
    let (body, check) = data.split_at(data.len() - 8);
    if blake3::hash(body).as_bytes()[..8] != *check {
        return None;
    }

    let mut at_op = [0u8; 32];
    at_op.copy_from_slice(&body[5..37]);
    let count = u32::from_le_bytes(body[37..41].try_into().ok()?) as usize;
    let mut refs = BTreeMap::new();
    let mut at = 41;
    for _ in 0..count {
        let name_len = u16::from_le_bytes(body.get(at..at + 2)?.try_into().ok()?) as usize;
        at += 2;
        let name = std::str::from_utf8(body.get(at..at + name_len)?).ok()?;
        at += name_len;
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
        refs.insert(name.to_owned(), target);
    }
    if at != body.len() {
        return None;
    }
    Some((OpId(at_op), refs))
}
