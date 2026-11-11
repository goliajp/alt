//! Verifies a received pack once and produces the index that makes it a segment.

use crate::{
    Error, FLAG_DUPLICATES, FLAG_OVERLAP, Kind, Oid, Record, SegmentSet,
    format::{self, CommitRow, Delta, NO_BASE},
    object::{self, Parsed},
    object_id,
    scan::{self, Form, Raw},
};
use std::{collections::HashMap, io::Write};

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub objects: u64,
    pub object_bytes: u64,
    pub inflated_bytes: u64,
    pub delta_depth: u16,
}

/// plain blobs above this size are hashed during the scan and not kept in memory
const KEEP: usize = 1 << 20;

struct Meta {
    oid: Oid,
    blake3: [u8; 32],
    kind: Kind,
    size: u64,
    depth: u16,
    /// scan index of the base inside this pack
    base: Option<usize>,
    outside: bool,
    parsed: Parsed,
}

pub struct Ingested {
    records: Vec<Record>,
    commits: Vec<CommitRow>,
    flags: u32,
    pack_len: u64,
}

impl Ingested {
    pub fn objects(&self) -> usize {
        self.records.len()
    }

    pub fn flags(&self) -> u32 {
        self.flags
    }

    pub fn contains(&self, oid: &Oid) -> bool {
        self.find(oid).is_some()
    }

    fn find(&self, oid: &Oid) -> Option<usize> {
        self.records
            .binary_search_by(|record| record.oid.cmp(oid))
            .ok()
    }

    pub fn kind(&self, oid: &Oid) -> Option<Kind> {
        self.find(oid).map(|index| self.records[index].kind)
    }

    /// parents and tree of a commit carried by this pack
    pub fn commit(&self, oid: &Oid) -> Option<(&Oid, &[Oid])> {
        self.commits
            .binary_search_by(|commit| commit.oid.cmp(oid))
            .ok()
            .map(|index| {
                (
                    &self.commits[index].tree,
                    self.commits[index].parents.as_slice(),
                )
            })
    }

    /// appends the index; the caller has already written the `pack_len` pack bytes
    pub fn write_index(&self, out: &mut impl Write) -> Result<(), Error> {
        format::write_index(&self.records, &self.commits, self.flags, self.pack_len, out)
    }
}

fn digest(kind: Kind, data: &[u8]) -> (Oid, [u8; 32]) {
    (
        object_id(kind, data),
        *alt_hash::blake3::hash(data).as_bytes(),
    )
}

struct Shared<'a> {
    entries: &'a [Raw],
    by_base: &'a HashMap<Oid, Vec<usize>>,
    limits: &'a Limits,
}

struct Frame {
    data: Vec<u8>,
    kind: Kind,
    depth: u16,
    index: Option<usize>,
    children: Vec<usize>,
    next: usize,
}

/// resolves every delta below one base; returns the metadata of each resolved entry
fn descend(
    shared: &Shared<'_>,
    offset_children: &HashMap<usize, Vec<usize>>,
    root: Frame,
) -> Result<Vec<(usize, Meta)>, Error> {
    let mut done = Vec::new();
    let mut stack = vec![root];
    while let Some(frame) = stack.last_mut() {
        let Some(child) = frame.children.get(frame.next).copied() else {
            stack.pop();
            continue;
        };
        frame.next += 1;
        let depth = frame.depth + 1;
        if depth > shared.limits.delta_depth {
            return Err(Error::Limit("delta depth"));
        }
        let raw = &shared.entries[child];
        let delta = raw.data.as_deref().ok_or(Error::Format("delta payload"))?;
        let data =
            alt_git_pack::delta::apply_bounded(&frame.data, delta, shared.limits.object_bytes)
                .map_err(|_| Error::Format("delta instructions"))?;
        let kind = frame.kind;
        let (oid, blake3) = digest(kind, &data);
        let meta = Meta {
            oid,
            blake3,
            kind,
            size: data.len() as u64,
            depth,
            base: frame.index,
            outside: frame.index.is_none(),
            parsed: object::parse(kind, &data)?,
        };
        let mut children = shared.by_base.get(&oid).cloned().unwrap_or_default();
        if let Some(more) = offset_children.get(&child) {
            children.extend_from_slice(more);
        }
        done.push((child, meta));
        if !children.is_empty() {
            stack.push(Frame {
                data,
                kind,
                depth,
                index: Some(child),
                children,
                next: 0,
            });
        }
    }
    Ok(done)
}

/// `pack` is the complete received pack including its trailer
pub fn ingest(pack: &[u8], existing: &SegmentSet, limits: &Limits) -> Result<Ingested, Error> {
    let (trailer, scanned) = alt_threads::join(
        || -> Oid { alt_hash::sha1(&pack[..pack.len().saturating_sub(20)]) },
        || {
            let mut large = HashMap::new();
            scan::scan(pack, limits, KEEP, |index, kind, data| {
                large.insert(index, (kind, digest(kind, data), data.len() as u64));
            })
            .map(|scanned| (scanned, large))
        },
    );
    let (scanned, large) = scanned?;
    if trailer[..] != pack[scanned.body_end..] {
        return Err(Error::Format("pack checksum"));
    }
    let entries = scanned.entries;
    let offsets: HashMap<u64, usize> = entries
        .iter()
        .enumerate()
        .map(|(index, raw)| (raw.offset, index))
        .collect();
    let mut offset_children: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut by_base: HashMap<Oid, Vec<usize>> = HashMap::new();
    for (index, raw) in entries.iter().enumerate() {
        match raw.form {
            Form::Plain(_) => {}
            Form::Offset(base) => {
                let base = *offsets
                    .get(&base)
                    .ok_or(Error::Format("delta base offset"))?;
                offset_children.entry(base).or_default().push(index);
            }
            Form::Reference(base) => by_base.entry(base).or_default().push(index),
        }
    }
    let shared = Shared {
        entries: &entries,
        by_base: &by_base,
        limits,
    };

    // Plain entries: object ids for a batch of objects are computed together, several
    // messages per step of the hash; batches run in parallel.
    let plain_indexes: Vec<usize> = entries
        .iter()
        .enumerate()
        .filter(|(_, raw)| matches!(raw.form, Form::Plain(_)))
        .map(|(index, _)| index)
        .collect();
    let batches: Vec<&[usize]> = plain_indexes.chunks(32).collect();
    let plain: Vec<(usize, Meta)> =
        alt_threads::map(&batches, 1, |batch| -> Result<Vec<(usize, Meta)>, Error> {
            let batch = *batch;
            let kept: Vec<(usize, Kind, &[u8])> = batch
                .iter()
                .filter_map(|index| {
                    let raw = &entries[*index];
                    let Form::Plain(kind) = raw.form else {
                        return None;
                    };
                    raw.data.as_deref().map(|data| (*index, kind, data))
                })
                .collect();
            let headers: Vec<String> = kept
                .iter()
                .map(|(_, kind, data)| crate::object_header(*kind, data.len()))
                .collect();
            let parts: Vec<[&[u8]; 2]> = headers
                .iter()
                .zip(&kept)
                .map(|(header, (_, _, data))| [header.as_bytes(), *data])
                .collect();
            let messages: Vec<&[&[u8]]> = parts.iter().map(|pair| pair.as_slice()).collect();
            let oids = alt_hash::sha1_many(&messages);
            let mut out = Vec::with_capacity(batch.len());
            for ((index, kind, data), oid) in kept.iter().zip(oids) {
                out.push((
                    *index,
                    Meta {
                        oid,
                        blake3: *alt_hash::blake3::hash(data).as_bytes(),
                        kind: *kind,
                        size: data.len() as u64,
                        depth: 0,
                        base: None,
                        outside: false,
                        parsed: object::parse(*kind, data)?,
                    },
                ));
            }
            for index in batch {
                // large blobs were hashed during the scan and are not kept in memory
                if entries[*index].data.is_none() {
                    let (kind, (oid, blake3), size) = large[index];
                    out.push((
                        *index,
                        Meta {
                            oid,
                            blake3,
                            kind,
                            size,
                            depth: 0,
                            base: None,
                            outside: false,
                            parsed: Parsed::Blob,
                        },
                    ));
                }
            }
            Ok(out)
        })
        .into_iter()
        .collect::<Result<Vec<_>, Error>>()?
        .into_iter()
        .flatten()
        .collect();
    let mut metas: Vec<Option<Meta>> = (0..entries.len()).map(|_| None).collect();
    let mut roots = Vec::new();
    for (index, meta) in plain {
        if offset_children.contains_key(&index) || by_base.contains_key(&meta.oid) {
            roots.push((index, meta.oid, meta.kind));
        }
        metas[index] = Some(meta);
    }
    let mut engine_roots = Vec::with_capacity(roots.len());
    for (index, oid, kind) in roots {
        let mut children = by_base.get(&oid).cloned().unwrap_or_default();
        if let Some(more) = offset_children.get(&index) {
            children.extend_from_slice(more);
        }
        engine_roots.push((index, kind, children));
    }
    let resolve = |roots: Vec<Frame>| -> Result<Vec<(usize, Meta)>, Error> {
        let parts: Vec<Vec<(usize, Meta)>> =
            alt_threads::map_owned(roots, 1, |root| descend(&shared, &offset_children, root))
                .into_iter()
                .collect::<Result<_, Error>>()?;
        Ok(parts.into_iter().flatten().collect())
    };
    let inside = engine_roots
        .into_iter()
        .map(|(index, kind, children)| {
            let raw = &entries[index];
            let data = match &raw.data {
                Some(data) => data.clone(),
                None => {
                    let start = raw.offset as usize + raw.header as usize;
                    scan::inflate(&pack[start..], raw.size as usize)?.0
                }
            };
            Ok(Frame {
                data,
                kind,
                depth: 0,
                index: Some(index),
                children,
                next: 0,
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    for (index, meta) in resolve(inside)? {
        metas[index].get_or_insert(meta);
    }
    // thin pack: bases that live in earlier segments
    let mut outside = Vec::new();
    for (base, children) in &by_base {
        if children.iter().all(|child| metas[*child].is_some()) {
            continue;
        }
        let (kind, data) = existing.read(base).map_err(|_| Error::MissingBase(*base))?;
        outside.push(Frame {
            data,
            kind,
            depth: 0,
            index: None,
            children: children.clone(),
            next: 0,
        });
    }
    for (index, meta) in resolve(outside)? {
        metas[index].get_or_insert(meta);
    }
    let metas: Vec<Meta> = metas
        .into_iter()
        .map(|meta| meta.ok_or(Error::Format("delta without a resolvable base")))
        .collect::<Result<_, Error>>()?;

    // records sorted by oid, first occurrence wins
    let mut order: Vec<usize> = (0..metas.len()).collect();
    order.sort_unstable_by(|a, b| metas[*a].oid.cmp(&metas[*b].oid).then(a.cmp(b)));
    let mut flags = 0;
    let mut kept: Vec<usize> = Vec::with_capacity(order.len());
    let mut position = vec![0u32; metas.len()];
    for index in order {
        match kept.last() {
            Some(last) if metas[*last].oid == metas[index].oid => flags |= FLAG_DUPLICATES,
            _ => kept.push(index),
        }
        position[index] = (kept.len() - 1) as u32;
    }
    let lookup = |oid: &Oid| {
        kept.binary_search_by(|index| metas[*index].oid.cmp(oid))
            .ok()
            .map(|at| &metas[kept[at]])
    };

    // every reference resolves inside the pack or in the segments already present
    let check = |from: &Oid, to: &Oid, kind: Kind| -> Result<(), Error> {
        let found = match lookup(to) {
            Some(meta) => Some(meta.kind),
            None => existing.kind(to)?,
        };
        if found == Some(kind) {
            Ok(())
        } else {
            Err(Error::Dangling(*from, *to))
        }
    };
    alt_threads::try_for_each(&kept, 1024, |index| {
        let meta = &metas[*index];
        match &meta.parsed {
            Parsed::Blob => Ok(()),
            Parsed::Commit { tree, parents } => {
                check(&meta.oid, tree, Kind::Tree)?;
                parents
                    .iter()
                    .try_for_each(|parent| check(&meta.oid, parent, Kind::Commit))
            }
            Parsed::Tree(entries) => entries
                .iter()
                .try_for_each(|(oid, kind)| check(&meta.oid, oid, *kind)),
            Parsed::Tag { object, kind } => check(&meta.oid, object, *kind),
        }
    })?;
    if alt_threads::any(&kept, 1024, |index| {
        existing.locate(&metas[*index].oid).is_some()
    }) {
        flags |= FLAG_OVERLAP;
    }

    // generation numbers: one more than the largest parent generation
    let commit_indexes: Vec<usize> = kept
        .iter()
        .copied()
        .filter(|index| metas[*index].kind == Kind::Commit)
        .collect();
    let slot: HashMap<Oid, usize> = commit_indexes
        .iter()
        .enumerate()
        .map(|(at, index)| (metas[*index].oid, at))
        .collect();
    let mut generation = vec![0u32; commit_indexes.len()];
    for start in 0..commit_indexes.len() {
        let mut stack = vec![start];
        while let Some(at) = stack.last().copied() {
            if generation[at] != 0 {
                stack.pop();
                continue;
            }
            let Parsed::Commit { parents, .. } = &metas[commit_indexes[at]].parsed else {
                unreachable!()
            };
            let mut highest = 0;
            let mut waiting = false;
            for parent in parents {
                match slot.get(parent) {
                    Some(parent_at) if generation[*parent_at] == 0 => {
                        if stack.contains(parent_at) {
                            return Err(Error::Format("commit ancestry cycle"));
                        }
                        stack.push(*parent_at);
                        waiting = true;
                    }
                    Some(parent_at) => highest = highest.max(generation[*parent_at]),
                    None => {
                        highest = highest.max(
                            existing
                                .commit(parent)
                                .map_or(0, |commit| commit.generation),
                        )
                    }
                }
            }
            if !waiting {
                generation[at] = highest + 1;
                stack.pop();
            }
        }
    }
    let commits = commit_indexes
        .iter()
        .zip(&generation)
        .map(|(index, generation)| {
            let Parsed::Commit { tree, parents } = &metas[*index].parsed else {
                unreachable!()
            };
            CommitRow {
                oid: metas[*index].oid,
                tree: *tree,
                generation: *generation,
                parents: parents.clone(),
            }
        })
        .collect();
    let records = kept
        .iter()
        .map(|index| {
            let (meta, raw) = (&metas[*index], &entries[*index]);
            Record {
                oid: meta.oid,
                blake3: meta.blake3,
                offset: raw.offset,
                size: meta.size,
                crc: raw.crc,
                len: raw.len,
                base: meta.base.map_or(NO_BASE, |base| position[base]),
                kind: meta.kind,
                delta: match (raw.form, meta.outside) {
                    (Form::Plain(_), _) => Delta::Plain,
                    (_, true) => Delta::Outside,
                    (_, false) => Delta::Inside,
                },
                depth: meta.depth,
            }
        })
        .collect();
    Ok(Ingested {
        records,
        commits,
        flags,
        pack_len: pack.len() as u64,
    })
}
