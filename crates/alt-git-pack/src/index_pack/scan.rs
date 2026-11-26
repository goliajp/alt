//! The first pass over a pack: where each entry lies, what it is a delta
//! of, and the id of each plain entry.

use alt_git_codec::{ObjectId, ObjectKind};

use super::{IndexLimits, IndexWork, ObjectMemory};
use crate::{EntryKind, Pack, PackError};

/// The fewest bytes an entry takes: a byte of header and the zlib stream
/// of nothing.
const ENTRY_MIN: usize = 9;

/// What indexing keeps of `entries` entries at once.
pub(super) fn held(entries: usize) -> u64 {
    (entries as u64).saturating_mul(super::HELD_FOR_AN_INDEXED_ENTRY)
}

/// [`held`] of the entries a pack that says it has `count` may have: no
/// more than its body holds.
pub(super) fn held_by(pack: &Pack, count: u32) -> u64 {
    let body = pack.bytes().len() - pack.algo().raw_len();
    held((count as usize).min(body / ENTRY_MIN))
}

pub(super) enum Form {
    Plain(ObjectKind),
    /// a delta of the entry at this index
    Ofs(u32),
    /// a delta of the object with this id, which it says has so many bytes
    Ref(ObjectId, u64),
}

pub(super) struct Entry {
    pub offset: u64,
    /// where its zlib stream starts
    pub data_at: u64,
    /// what the stream inflates to: the object of a plain entry, the
    /// instructions of a delta
    pub size: u64,
    pub form: Form,
    /// of the raw bytes, header and stream
    pub crc32: u32,
}

pub(super) struct Scanned {
    pub entries: Vec<Entry>,
    /// the id of each plain entry
    pub oids: Vec<Option<ObjectId>>,
    /// what the entries inflate to, all together
    pub inflated: u64,
    /// the most bytes one entry inflates to, or the object of a delta is
    /// said to have
    pub largest: u64,
    pub body_end: usize,
}

pub(super) fn scan(
    pack: &Pack,
    count: u32,
    limits: &IndexLimits<'_>,
    objects: Option<ObjectMemory<'_>>,
    work: &mut IndexWork,
) -> Result<Scanned, PackError> {
    let map = pack.bytes();
    let body_end = map.len() - pack.algo().raw_len();
    // the count is as the sender wrote it: room is made for no more
    // entries than the body can hold
    let room = (count as usize).min(body_end / ENTRY_MIN);
    let mut entries: Vec<Entry> = Vec::with_capacity(room);
    let mut oids = Vec::with_capacity(room);
    let mut inflated = 0u64;
    let mut largest = 0u64;
    let mut payload = Vec::new();
    // the memory of the entry being inflated, where it is a large one
    let mut room = None;
    let mut pos = 12usize;
    for _ in 0..count {
        limits.going_on()?;
        if pos >= body_end {
            return Err(PackError::Format("pack header count exceeds body"));
        }
        let info = pack.entry_info(pos as u64)?;
        if info.size > limits.object_bytes {
            return Err(PackError::Limit("object bytes"));
        }
        inflated = inflated
            .checked_add(info.size)
            .filter(|total| *total <= limits.inflated_bytes)
            .ok_or(PackError::Limit("inflated bytes"))?;
        if let Some(objects) = &objects {
            objects.room(&mut room, info.size, limits.wait())?;
        }
        let next = pack.inflate_into(info.data_at, info.size, body_end, &mut payload)?;
        work.inflated += 1;
        let made = match info.kind {
            EntryKind::Plain(_) => 0,
            _ => crate::delta::target_size(&payload)?.min(limits.object_bytes),
        };
        largest = largest.max(info.size).max(made);
        let (form, oid) = match info.kind {
            EntryKind::Plain(kind) => {
                work.hashed += 1;
                let oid = ObjectId::hash_object(pack.algo(), kind, &payload);
                (Form::Plain(kind), Some(oid))
            }
            // entries are in file order: the base lies before this one
            EntryKind::OfsDelta { base_at } => {
                let base = entries
                    .binary_search_by_key(&base_at, |entry| entry.offset)
                    .map_err(|_| PackError::Format("ofs-delta base offset not found"))?;
                (Form::Ofs(base as u32), None)
            }
            EntryKind::RefDelta { base } => {
                let size = crate::delta::source_size(&payload)?;
                (Form::Ref(base, size), None)
            }
        };
        entries.push(Entry {
            offset: pos as u64,
            data_at: info.data_at,
            size: info.size,
            form,
            crc32: alt_hash::crc32(&map[pos..next]),
        });
        oids.push(oid);
        pos = next;
    }
    if pos != body_end {
        return Err(PackError::Format("pack body has trailing bytes"));
    }
    Ok(Scanned {
        entries,
        oids,
        inflated,
        largest,
        body_end,
    })
}
