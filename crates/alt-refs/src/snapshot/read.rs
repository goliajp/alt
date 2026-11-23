//! Reading a snapshot file back.

use std::collections::BTreeMap;
use std::fs::File;
use std::path::Path;

use alt_git_codec::{HashAlgo, ObjectId};
use alt_oplog::{Anchor, OpId};
use alt_os::map::Mmap;

use super::{
    ANCHOR_LEN, FRAMED_AT, MAGIC, NOTE_AT, NOTE_END, Snapshot, TARGET_OID, TARGET_SYMBOLIC,
    VERSION, VERSION_FRAMED, VERSION_NO_NOTE, VERSION_OP_ONLY,
};
use crate::RefTarget;
use crate::state::{BLOCK, Base, Refs};

/// One ref at `data[*at..]`; advances `at` past it.
pub(crate) fn parse_entry<'a>(data: &'a [u8], at: &mut usize) -> Option<(&'a str, RefTarget)> {
    let name_len = u16::from_le_bytes(data.get(*at..*at + 2)?.try_into().ok()?) as usize;
    *at += 2;
    let name = std::str::from_utf8(data.get(*at..*at + name_len)?).ok()?;
    *at += name_len;
    let target = match *data.get(*at)? {
        TARGET_OID => {
            let algo = match *data.get(*at + 1)? {
                1 => HashAlgo::Sha1,
                2 => HashAlgo::Sha256,
                _ => return None,
            };
            *at += 2;
            let oid = ObjectId::from_bytes(algo, data.get(*at..*at + algo.raw_len())?).ok()?;
            *at += algo.raw_len();
            RefTarget::Oid(oid)
        }
        TARGET_SYMBOLIC => {
            let len = u16::from_le_bytes(data.get(*at + 1..*at + 3)?.try_into().ok()?) as usize;
            *at += 3;
            let sym = std::str::from_utf8(data.get(*at..*at + len)?).ok()?;
            *at += len;
            RefTarget::Symbolic(sym.to_owned())
        }
        _ => return None,
    };
    Some((name, target))
}

fn field(body: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(body.get(at..at + 8)?.try_into().ok()?))
}

/// The refs of a version 1 file: a count, then every ref up to the end.
fn refs_op_only(body: &[u8]) -> Option<Refs> {
    let count = u32::from_le_bytes(body[37..41].try_into().unwrap()) as usize;
    let mut refs = BTreeMap::new();
    let mut at = 41;
    for _ in 0..count {
        let (name, target) = parse_entry(body, &mut at)?;
        refs.insert(name.to_owned(), Some(target));
    }
    if at != body.len() {
        return None;
    }
    Some(Refs::from_changes(refs))
}

/// The anchor that begins at `at` of an anchored snapshot's `body`, where
/// the table of its ref blocks begins, and how many refs it holds.
fn anchored(body: &[u8], at: usize) -> Option<(Anchor, usize, usize)> {
    let anchor = Anchor {
        op: OpId(body.get(at..at + 32)?.try_into().ok()?),
        start: field(body, at + 32)?,
        end: field(body, at + 40)?,
        count: field(body, at + 48)?,
    };
    let counts = at + ANCHOR_LEN;
    let count = u32::from_le_bytes(body.get(counts..counts + 4)?.try_into().ok()?);
    let blocks = u32::from_le_bytes(body.get(counts + 4..counts + 8)?.try_into().ok()?);
    let table = counts + 8;
    let entries = table.checked_add((blocks as usize).checked_mul(4)?)?;
    if blocks as usize != (count as usize).div_ceil(BLOCK) || entries > body.len() {
        return None;
    }
    Some((anchor, table, count as usize))
}

/// Reads a snapshot; any structural problem yields None (rebuild), never an
/// error — the snapshot is a cache.
pub fn read(path: &Path) -> Option<Snapshot> {
    let map = unsafe { Mmap::map(&File::open(path).ok()?).ok()? };
    if map.len() < 5 + 32 + 4 + 8 || map[..4] != MAGIC {
        return None;
    }
    let version = map[4];
    let len = map.len() as u64;
    let body_len = map.len() - 8;
    // the note is changed in place: it carries its own checksum
    let (from, synced) = match version {
        VERSION => (NOTE_END, alt_oplog::parse_note(map.get(NOTE_AT..NOTE_END)?)),
        VERSION_FRAMED => (0, field(&map, 21)),
        _ => (0, None),
    };
    let boot = match version {
        VERSION_FRAMED => Some(map.get(5..21)?.try_into().ok()?),
        _ => None,
    };
    let checked = map.get(from..body_len)?;
    if alt_hash::blake3::hash(checked).as_bytes()[..8] != map[body_len..] {
        return None;
    }
    let body = &map[..body_len];

    if version == VERSION_OP_ONLY {
        return Some(Snapshot {
            at_op: OpId(body[5..37].try_into().unwrap()),
            anchor: None,
            refs: refs_op_only(body)?,
            len,
            synced,
            boot,
        });
    }
    let at = match version {
        VERSION => NOTE_END,
        VERSION_FRAMED => FRAMED_AT,
        VERSION_NO_NOTE => 5,
        _ => return None,
    };
    let (anchor, table, count) = anchored(body, at)?;
    let refs = Base::new(map, table, count, body_len);
    Some(Snapshot {
        at_op: anchor.op,
        anchor: Some(anchor),
        refs: Refs::from_base(refs),
        len,
        synced,
        boot,
    })
}
