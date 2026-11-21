//! Ref-state snapshot: pure replay acceleration, never truth. The snapshot
//! records the full ref map as of one op; opening starts there and replays
//! only later ops. A missing, corrupt, or orphaned (op no longer in the
//! log) snapshot is simply ignored and rebuilt — exactly like the pack idx,
//! the oplog stays the single source of state.
//!
//! Version 3 (all integers little-endian):
//!
//! ```text
//! [magic "ALTS"][version u8]
//! [note 16]                         how far the log is known synced
//! [op id 32][record start u64][record end u64][op count u64]
//! [ref count u32][block count u32]
//! block count x [offset u32]        where each block of 64 refs starts
//! refs, names ascending: [name len u16][name][target]
//! [checksum 8]                      over everything between it and the note
//! ```
//!
//! The op's place in the log lets the log be read from there on; the block
//! offsets let a lookup parse one block instead of every ref. The note
//! (see [`alt_oplog::NotePlace`]) is the one part a writer of the log
//! changes in place, after it has synced the log: an open whose log is no
//! longer than the note says does not read the log's marker. Version 2
//! files (no note) and version 1 files (op id and refs only) are still
//! read; a version 1 file costs one full replay.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::Path;

use alt_git_codec::{HashAlgo, ObjectId};
use alt_oplog::{Anchor, OpId};
use alt_os::map::Mmap;

use crate::state::{BLOCK, Base, Refs};
use crate::{RefError, RefTarget};

const MAGIC: [u8; 4] = *b"ALTS";
const VERSION_OP_ONLY: u8 = 1;
const VERSION_NO_NOTE: u8 = 2;
const VERSION: u8 = 3;
const NOTE_AT: usize = 5;
const NOTE_END: usize = NOTE_AT + alt_oplog::NOTE_LEN;
/// op id, record start, record end, op count
const ANCHOR_LEN: usize = 32 + 24;

/// Where a writer of the log notes in the snapshot at `path` how far the
/// log is synced.
pub fn note_place(path: &Path) -> alt_oplog::NotePlace {
    let mut tag = MAGIC.to_vec();
    tag.push(VERSION);
    alt_oplog::NotePlace {
        path: path.to_owned(),
        tag,
        at: NOTE_AT as u64,
    }
}

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
    /// how far the file notes the log as synced; absent before version 3
    pub synced: Option<u64>,
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

/// The bytes of a snapshot of `count` refs (in name order) that notes the
/// log as synced up to `synced`.
fn encode<'a>(
    at: &Anchor,
    synced: u64,
    count: usize,
    refs: impl Iterator<Item = (&'a str, &'a RefTarget)>,
) -> Vec<u8> {
    let blocks = count.div_ceil(BLOCK);
    let mut buf = Vec::new();
    buf.extend_from_slice(&MAGIC);
    buf.push(VERSION);
    buf.extend_from_slice(&alt_oplog::encode_note(synced));
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
    let check: [u8; 8] = alt_hash::blake3::hash(&buf[NOTE_END..]).as_bytes()[..8]
        .try_into()
        .unwrap();
    buf.extend_from_slice(&check);
    buf
}

/// Writes the snapshot of `count` refs (in name order), with the note
/// that the log is synced up to `synced`, and returns its size. No fsync:
/// a snapshot lost or torn in a crash fails its checksum and is rebuilt
/// from the log.
pub fn write<'a>(
    path: &Path,
    at: &Anchor,
    synced: u64,
    count: usize,
    refs: impl Iterator<Item = (&'a str, &'a RefTarget)>,
) -> Result<u64, RefError> {
    let buf = encode(at, synced, count, refs);

    // readers refresh the snapshot too, any number at once, in one process
    // or in several that may share process ids: each writes a file no one
    // else can have, created exclusively
    let mut nonce = [0u8; 8];
    alt_os::random::fill(&mut nonce);
    let tmp = path.with_extension(format!("tmp.{:016x}", u64::from_le_bytes(nonce)));
    let written = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .and_then(|mut file| file.write_all(&buf))
        .and_then(|()| fs::rename(&tmp, path));
    if let Err(e) = written {
        let _ = fs::remove_file(&tmp);
        return Err(e.into());
    }
    remove_leftovers(path);
    Ok(buf.len() as u64)
}

/// A writer that died between creating its temporary file and renaming it
/// left that file behind. Snapshots are written without a lock, so another
/// writer's file may be in use right now: only ones older than any write
/// takes are removed.
fn remove_leftovers(path: &Path) {
    const DEAD_AFTER: std::time::Duration = std::time::Duration::from_secs(60);
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return;
    };
    let mut prefix = name.as_encoded_bytes().to_vec();
    prefix.extend_from_slice(b".tmp.");
    let Ok(listing) = fs::read_dir(dir) else {
        return;
    };
    for entry in listing.flatten() {
        let dead = entry.file_name().as_encoded_bytes().starts_with(&prefix)
            && entry
                .metadata()
                .and_then(|meta| meta.modified())
                .is_ok_and(|at| at.elapsed().is_ok_and(|age| age > DEAD_AFTER));
        if dead {
            let _ = fs::remove_file(entry.path());
        }
    }
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
        _ => (0, None),
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
        });
    }
    if version != VERSION && version != VERSION_NO_NOTE {
        return None;
    }
    let at = if version == VERSION { NOTE_END } else { 5 };
    let (anchor, table, count) = anchored(body, at)?;
    let refs = Base::new(map, table, count, body_len);
    Some(Snapshot {
        at_op: anchor.op,
        anchor: Some(anchor),
        refs: Refs::from_base(refs),
        len,
        synced,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn verifies(map: &[u8]) -> bool {
        map.len() > NOTE_END + 8
            && alt_hash::blake3::hash(&map[NOTE_END..map.len() - 8]).as_bytes()[..8]
                == map[map.len() - 8..]
    }

    /// Two stores in one process (two threads) refresh the same snapshot:
    /// every write goes through, and a snapshot a reader has mapped and
    /// verified is never written to again.
    #[test]
    fn concurrent_writers_in_one_process_never_share_a_temporary_file() {
        let dir = alt_os::temp::tempdir().unwrap();
        let path = dir.path().join("snapshot");
        let stop = Arc::new(AtomicBool::new(false));
        let writers: Vec<_> = (0..2u8)
            .map(|writer| {
                let path = path.clone();
                std::thread::spawn(move || {
                    for round in 0..400u64 {
                        let refs: Vec<(String, RefTarget)> = (0..50 + round % 200)
                            .map(|n| {
                                let target =
                                    RefTarget::Symbolic(format!("refs/x/{writer}/{round}"));
                                (format!("refs/heads/b{n:04}"), target)
                            })
                            .collect();
                        let anchor = Anchor {
                            op: OpId([writer; 32]),
                            start: round,
                            end: round + 1,
                            count: round + 1,
                        };
                        let listed = refs.iter().map(|(name, target)| (name.as_str(), target));
                        write(&path, &anchor, round, refs.len(), listed)
                            .unwrap_or_else(|e| panic!("writer {writer} round {round}: {e:?}"));
                    }
                })
            })
            .collect();
        let reader = {
            let (path, stop) = (path.clone(), Arc::clone(&stop));
            std::thread::spawn(move || {
                let mut held = Vec::new();
                while !stop.load(Ordering::Relaxed) {
                    let Ok(file) = File::open(&path) else {
                        continue;
                    };
                    let map = unsafe { Mmap::map(&file).unwrap() };
                    if verifies(&map) {
                        held.push(map);
                    }
                    if held.len() > 64 {
                        held.remove(0);
                    }
                    for map in &held {
                        assert!(verifies(map), "a snapshot changed after it was read");
                    }
                }
            })
        };
        for writer in writers {
            writer.join().unwrap();
        }
        stop.store(true, Ordering::Relaxed);
        reader.join().unwrap();
        assert!(read(&path).is_some());
        let left: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(left, ["snapshot"], "no temporary file is left behind");
    }
}
