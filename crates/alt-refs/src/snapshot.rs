//! Ref-state snapshot: pure replay acceleration, never truth. The snapshot
//! records the full ref map as of one op; opening starts there and replays
//! only later ops. A missing, corrupt, or orphaned (op no longer in the
//! log) snapshot is simply ignored and rebuilt — exactly like the pack idx,
//! the oplog stays the single source of state.
//!
//! Version 2 (all integers little-endian):
//!
//! ```text
//! [magic "ALTS"][version u8]
//! [op id 32][record start u64][record end u64][op count u64]
//! [ref count u32][block count u32]
//! block count x [offset u32]        where each block of 64 refs starts
//! refs, names ascending: [name len u16][name][target]
//! [checksum 8]                      over everything before it
//! ```
//!
//! The op's place in the log lets the log be read from there on; the block
//! offsets let a lookup parse one block instead of every ref. Version 1
//! files (op id and refs only) are still read; they cost one full replay,
//! after which a version 2 file replaces them.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::Path;

use alt_git_codec::{HashAlgo, ObjectId};
use alt_oplog::{Anchor, OpId};
use alt_os::map::Mmap;

use crate::state::{BLOCK, Base, Refs};
use crate::{RefError, RefTarget};

const MAGIC: [u8; 4] = *b"ALTS";
const VERSION_OP_ONLY: u8 = 1;
const VERSION: u8 = 2;
/// magic, version, op id, record start, record end, op count
const ANCHOR_END: usize = 5 + 32 + 24;

const TARGET_OID: u8 = 1;
const TARGET_SYMBOLIC: u8 = 2;

/// What a snapshot file holds.
pub struct Snapshot {
    pub at_op: OpId,
    /// where `at_op` sits in the log; absent in version 1 files
    pub anchor: Option<Anchor>,
    pub refs: Refs,
    /// size of the file
    pub len: u64,
}

fn encode_target(buf: &mut Vec<u8>, target: &RefTarget) {
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

/// Writes the snapshot of `count` refs (in name order) and returns its
/// size. No fsync: a snapshot lost or torn in a crash fails its checksum
/// and is rebuilt from the log.
pub fn write<'a>(
    path: &Path,
    at: &Anchor,
    count: usize,
    refs: impl Iterator<Item = (&'a str, &'a RefTarget)>,
) -> Result<u64, RefError> {
    let blocks = count.div_ceil(BLOCK);
    let mut buf = Vec::new();
    buf.extend_from_slice(&MAGIC);
    buf.push(VERSION);
    buf.extend_from_slice(&at.op.0);
    buf.extend_from_slice(&at.start.to_le_bytes());
    buf.extend_from_slice(&at.end.to_le_bytes());
    buf.extend_from_slice(&at.count.to_le_bytes());
    buf.extend_from_slice(&(count as u32).to_le_bytes());
    buf.extend_from_slice(&(blocks as u32).to_le_bytes());
    let table = buf.len();
    buf.resize(table + blocks * 4, 0);
    for (i, (name, target)) in refs.enumerate() {
        if i % BLOCK == 0 {
            let at = table + i / BLOCK * 4;
            let offset = buf.len() as u32;
            buf[at..at + 4].copy_from_slice(&offset.to_le_bytes());
        }
        buf.extend_from_slice(&(name.len() as u16).to_le_bytes());
        buf.extend_from_slice(name.as_bytes());
        encode_target(&mut buf, target);
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
    if alt_hash::blake3::hash(&map[..body_len]).as_bytes()[..8] != map[body_len..] {
        return None;
    }
    let body = &map[..body_len];
    let at_op = OpId(body[5..37].try_into().unwrap());

    if version == VERSION_OP_ONLY {
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
        return Some(Snapshot {
            at_op,
            anchor: None,
            refs: Refs::from_changes(refs),
            len,
        });
    }
    if version != VERSION {
        return None;
    }
    let anchor = Anchor {
        op: at_op,
        start: field(body, 37)?,
        end: field(body, 45)?,
        count: field(body, 53)?,
    };
    let count = u32::from_le_bytes(body.get(ANCHOR_END..ANCHOR_END + 4)?.try_into().ok()?);
    let blocks = u32::from_le_bytes(body.get(ANCHOR_END + 4..ANCHOR_END + 8)?.try_into().ok()?);
    let table = ANCHOR_END + 8;
    let entries = table.checked_add((blocks as usize).checked_mul(4)?)?;
    if blocks as usize != (count as usize).div_ceil(BLOCK) || entries > body.len() {
        return None;
    }
    let refs = Base::new(map, table, count as usize, body_len);
    Some(Snapshot {
        at_op,
        anchor: Some(anchor),
        refs: Refs::from_base(refs),
        len,
    })
}
