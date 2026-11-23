//! Resolves the deltas below one base of a received pack.

use crate::{
    Error, Kind, OidMap,
    ingest::{Limits, Meta, digest},
    object,
    scan::{self, Raw},
};
use std::{
    borrow::Cow,
    collections::HashMap,
    sync::atomic::{AtomicU64, Ordering},
};

pub(crate) struct Shared<'a> {
    pub pack: &'a [u8],
    pub entries: &'a [Raw],
    pub by_base: &'a OidMap<Vec<usize>>,
    pub limits: &'a Limits,
    /// bytes of the objects that deltas made and further deltas wait for, on every
    /// thread together
    pub waiting: &'a AtomicU64,
}

pub(crate) struct Frame {
    data: Vec<u8>,
    kind: Kind,
    depth: u16,
    index: Option<usize>,
    children: Vec<usize>,
    next: usize,
    /// whether its bytes are among those that wait
    made: bool,
}

impl Frame {
    /// a base that no delta of this pack made: a plain entry, or an object of an
    /// earlier segment
    pub(crate) fn base(
        data: Vec<u8>,
        kind: Kind,
        depth: u16,
        index: Option<usize>,
        children: Vec<usize>,
    ) -> Self {
        Frame {
            data,
            kind,
            depth,
            index,
            children,
            next: 0,
            made: false,
        }
    }

    /// The object the delta at `index` made, which further deltas wait for. A base
    /// that has more deltas than the one descended into stays: a pack can be made so
    /// that every link of a chain does, and the objects that wait are then as many as
    /// the chain is deep. They count against what the pack may inflate to.
    fn made(
        shared: &Shared<'_>,
        (data, kind, depth): (Vec<u8>, Kind, u16),
        index: usize,
        children: Vec<usize>,
    ) -> Result<Self, Error> {
        let bytes = data.len() as u64;
        let waiting = shared.waiting.fetch_add(bytes, Ordering::Relaxed);
        if waiting + bytes > shared.limits.inflated_bytes {
            return Err(Error::Limit("delta bases"));
        }
        let made = Frame::base(data, kind, depth, Some(index), children);
        Ok(Frame { made: true, ..made })
    }

    fn over(self, shared: &Shared<'_>) {
        if self.made {
            let bytes = self.data.len() as u64;
            shared.waiting.fetch_sub(bytes, Ordering::Relaxed);
        }
    }
}

/// the instructions of the delta at `index`
fn delta<'a>(shared: &Shared<'a>, index: usize) -> Result<Cow<'a, [u8]>, Error> {
    let raw = &shared.entries[index];
    Ok(match &raw.data {
        Some(delta) => Cow::Borrowed(delta.as_slice()),
        // not kept by the scan
        None => {
            let start = raw.offset as usize + raw.header as usize;
            Cow::Owned(scan::inflate(&shared.pack[start..], raw.size as usize)?.0)
        }
    })
}

/// resolves every delta below one base; returns the metadata of each resolved entry
pub(crate) fn descend(
    shared: &Shared<'_>,
    offset_children: &HashMap<usize, Vec<usize>>,
    root: Frame,
) -> Result<Vec<(usize, Meta)>, Error> {
    let mut done = Vec::new();
    let mut stack = vec![root];
    while let Some(frame) = stack.last_mut() {
        let Some(child) = frame.children.get(frame.next).copied() else {
            stack.pop().expect("the frame").over(shared);
            continue;
        };
        frame.next += 1;
        let depth = frame.depth + 1;
        if depth > shared.limits.delta_depth {
            return Err(Error::Limit("delta depth"));
        }
        let delta = delta(shared, child)?;
        let data =
            alt_git_pack::delta::apply_bounded(&frame.data, &delta, shared.limits.object_bytes)
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
        // a base is dropped before its last child is descended into, so a chain holds
        // one object and not one per link
        if frame.next == frame.children.len() {
            stack.pop().expect("the frame").over(shared);
        }
        if !children.is_empty() {
            stack.push(Frame::made(shared, (data, kind, depth), child, children)?);
        }
    }
    Ok(done)
}
