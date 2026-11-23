//! The second pass: every delta resolved to its object and hashed, from
//! its base onwards.

use std::sync::Arc;

use alt_git_codec::{ObjectId, ObjectKind};

use super::scan::{Entry, Form, Scanned};
use super::{IndexLimits, IndexWork, MAX_DELTA_DEPTH};
use crate::{Pack, PackError, delta};

/// A base on the path from a plain entry to the delta being resolved.
struct Frame {
    entry: u32,
    /// let go of when the bases held pass their limit
    data: Option<Arc<Vec<u8>>>,
    /// its deltas by offset and by id that are still to be resolved, as
    /// places in the sorted lists
    ofs: std::ops::Range<usize>,
    refs: std::ops::Range<usize>,
}

struct Resolver<'a> {
    pack: &'a Pack,
    entries: &'a [Entry],
    body_end: usize,
    limits: &'a IndexLimits,
    work: &'a mut IndexWork,
    /// `(base, delta)` by entry, in the order of the bases
    by_offset: Vec<(u32, u32)>,
    /// `(base, delta)`, in the order of the ids of the bases
    by_id: Vec<(ObjectId, u32)>,
    oids: Vec<Option<ObjectId>>,
    inflated: u64,
    stack: Vec<Frame>,
    /// bytes of the bases the stack holds
    held: usize,
    /// no frame below this one holds its base
    lowest: usize,
    scratch: Vec<u8>,
}

/// The id of every entry of the pack, in the order of its entries.
pub(super) fn resolve(
    pack: &Pack,
    scanned: &Scanned,
    limits: &IndexLimits,
    work: &mut IndexWork,
) -> Result<Vec<ObjectId>, PackError> {
    let entries = &scanned.entries;
    let mut by_offset = Vec::new();
    let mut by_id = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        match &entry.form {
            Form::Plain(_) => {}
            Form::Ofs(base) => by_offset.push((*base, index as u32)),
            Form::Ref(base) => by_id.push((*base, index as u32)),
        }
    }
    by_offset.sort_unstable();
    by_id.sort_unstable_by(|a, b| (a.0.as_bytes(), a.1).cmp(&(b.0.as_bytes(), b.1)));
    let mut resolver = Resolver {
        pack,
        entries,
        body_end: scanned.body_end,
        limits,
        work,
        by_offset,
        by_id,
        oids: scanned.oids.clone(),
        inflated: scanned.inflated,
        stack: Vec::new(),
        held: 0,
        lowest: 0,
        scratch: Vec::new(),
    };
    for (index, entry) in entries.iter().enumerate() {
        if let Form::Plain(kind) = entry.form {
            resolver.below(index as u32, kind)?;
        }
    }
    let stuck = PackError::Format("delta resolution stuck (ref-delta base missing or cycle)");
    resolver
        .oids
        .into_iter()
        .collect::<Option<_>>()
        .ok_or(stuck)
}

impl Resolver<'_> {
    /// A frame for the entry `index`, which is the object `oid`, when
    /// any delta is made of it.
    fn frame(&self, index: u32, oid: &ObjectId, data: Option<Arc<Vec<u8>>>) -> Option<Frame> {
        let ofs = self.by_offset.partition_point(|(base, _)| *base < index)
            ..self.by_offset.partition_point(|(base, _)| *base <= index);
        let id = oid.as_bytes();
        let refs = self.by_id.partition_point(|(base, _)| base.as_bytes() < id)
            ..self
                .by_id
                .partition_point(|(base, _)| base.as_bytes() <= id);
        (!ofs.is_empty() || !refs.is_empty()).then_some(Frame {
            entry: index,
            data,
            ofs,
            refs,
        })
    }

    fn inflate(&mut self, index: u32) -> Result<(), PackError> {
        let entry = &self.entries[index as usize];
        self.work.inflated += 1;
        let scratch = &mut self.scratch;
        self.pack
            .inflate_into(entry.data_at, entry.size, self.body_end, scratch)
            .map(|_| ())
    }

    fn apply(&mut self, base: &[u8], index: u32) -> Result<Vec<u8>, PackError> {
        self.inflate(index)?;
        self.work.applied += 1;
        delta::apply_bounded(base, &self.scratch, self.limits.object_bytes)
    }

    /// Resolves every delta below the plain entry `root`.
    fn below(&mut self, root: u32, kind: ObjectKind) -> Result<(), PackError> {
        let oid = self.oids[root as usize].expect("hashed by the scan");
        let Some(frame) = self.frame(root, &oid, None) else {
            return Ok(());
        };
        self.stack.push(frame);
        self.lowest = 0;
        while let Some(top) = self.stack.last_mut() {
            let next = (top.ofs.next().map(|at| self.by_offset[at].1))
                .or_else(|| top.refs.next().map(|at| self.by_id[at].1));
            let Some(child) = next else {
                let done = self.stack.pop().expect("the top");
                self.held -= done.data.map_or(0, |data| data.len());
                self.lowest = self.lowest.min(self.stack.len());
                continue;
            };
            // reached before, through another entry of the same object
            if self.oids[child as usize].is_some() {
                continue;
            }
            if self.stack.len() as u32 > MAX_DELTA_DEPTH {
                return Err(PackError::Limit("delta depth"));
            }
            let base = self.base()?;
            let data = self.apply(&base, child)?;
            drop(base);
            self.inflated = (self.inflated.checked_add(data.len() as u64))
                .filter(|total| *total <= self.limits.inflated_bytes)
                .ok_or(PackError::Limit("inflated bytes"))?;
            self.work.hashed += 1;
            let oid = ObjectId::hash_object(self.pack.algo(), kind, &data);
            self.oids[child as usize] = Some(oid);
            if let Some(mut frame) = self.frame(child, &oid, None) {
                self.held += data.len();
                frame.data = Some(Arc::new(data));
                self.stack.push(frame);
                self.let_go();
            }
        }
        Ok(())
    }

    /// The object of the top frame. Where it was let go of it is made
    /// again from the nearest base below it that is held, or from the
    /// pack, and the bases made on the way are held anew within the
    /// limit: the nearest ones are those the next deltas need.
    fn base(&mut self) -> Result<Arc<Vec<u8>>, PackError> {
        let top = self.stack.len() - 1;
        let from = (0..=top).rev().find(|at| self.stack[*at].data.is_some());
        let (mut data, from) = match from {
            Some(at) => (self.stack[at].data.clone().expect("found held"), at + 1),
            None => {
                self.inflate(self.stack[0].entry)?;
                (Arc::new(self.scratch.clone()), 0)
            }
        };
        for at in from..=top {
            if at > 0 {
                data = Arc::new(self.apply(&data, self.stack[at].entry)?);
            }
            self.held += data.len();
            self.stack[at].data = Some(data.clone());
            self.lowest = self.lowest.min(at);
            self.let_go();
        }
        Ok(data)
    }

    /// Lets go of bases while they pass their limit, those nearest the
    /// plain entry first: they are the last to be needed again. The top
    /// frame keeps its own.
    fn let_go(&mut self) {
        let top = self.stack.len() - 1;
        self.work.held = self.work.held.max(self.held);
        while self.held > self.limits.base_bytes && self.lowest < top {
            if let Some(data) = self.stack[self.lowest].data.take() {
                self.held -= data.len();
            }
            self.lowest += 1;
        }
    }
}
