//! The second pass: every delta resolved to its object and hashed, from
//! its base onwards.
//!
//! An object is held while deltas of it are still to be resolved, and no
//! longer: it is let go of when the last of them is taken up. The deltas
//! of an object are taken in an order that keeps few objects waiting:
//! those named by id first, then those named by offset, the one with the
//! most entries under it last. Under a chain whose objects each have a
//! further delta, the shape a history of one file has, nothing waits
//! then but the object being worked on.
//!
//! What still has to wait is held within the limit of the bases. An
//! object that passes it while deltas of it wait is written to a file of
//! its own, within a limit too, and read from there when it is come back
//! to. One there is no room for either is let go of and made again: that
//! work is the sender's to cause, and each object made or inflated again
//! counts against what the pack may inflate to, like the objects
//! themselves.

use std::sync::Arc;

use alt_git_codec::{ObjectId, ObjectKind};

use super::held::Base;
use super::scan::{Entry, Form, Scanned};
use super::{IndexLimits, IndexWork, MAX_DELTA_DEPTH};
use crate::{Pack, PackError, delta};

/// A base on the path from a plain entry to the delta being resolved.
struct Frame {
    entry: u32,
    /// let go of when the bases held pass their limit
    data: Option<Arc<Base>>,
    /// whether its bytes were there once: inflating them again is counted
    made: bool,
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
    /// bytes of the bases the stack holds, and of those it has in files
    held: usize,
    spilled: u64,
    /// no frame below this one holds its base
    lowest: usize,
    scratch: Vec<u8>,
}

/// Gives every delta under the plain entries from `roots` on its id in
/// `oids`, which are in the order of the entries. A delta whose base is
/// no object of the pack is left without one.
pub(super) fn resolve(
    pack: &Pack,
    scanned: &mut Scanned,
    oids: &mut Vec<Option<ObjectId>>,
    roots: usize,
    limits: &IndexLimits,
    work: &mut IndexWork,
) -> Result<(), PackError> {
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
    // how many entries rest on each by offset, itself included: a base
    // lies before its deltas
    let mut under = vec![1u32; entries.len()];
    for (index, entry) in entries.iter().enumerate().rev() {
        if let Form::Ofs(base) = entry.form {
            under[base as usize] = under[base as usize].saturating_add(under[index]);
        }
    }
    by_offset.sort_unstable_by_key(|(base, delta)| (*base, under[*delta as usize], *delta));
    drop(under);
    by_id.sort_unstable_by(|a, b| (a.0.as_bytes(), a.1).cmp(&(b.0.as_bytes(), b.1)));
    let mut resolver = Resolver {
        pack,
        entries,
        body_end: scanned.body_end,
        limits,
        work,
        by_offset,
        by_id,
        oids: std::mem::take(oids),
        inflated: scanned.inflated,
        stack: Vec::new(),
        held: 0,
        spilled: 0,
        lowest: 0,
        scratch: Vec::new(),
    };
    for (index, entry) in entries.iter().enumerate().skip(roots) {
        if let Form::Plain(kind) = entry.form {
            resolver.below(index as u32, kind)?;
        }
    }
    *oids = resolver.oids;
    scanned.inflated = resolver.inflated;
    Ok(())
}

impl Resolver<'_> {
    /// A frame for the entry `index`, which is the object `oid`, when
    /// any delta is made of it.
    fn frame(&self, index: u32, oid: &ObjectId) -> Option<Frame> {
        let ofs = self.by_offset.partition_point(|(base, _)| *base < index)
            ..self.by_offset.partition_point(|(base, _)| *base <= index);
        let id = oid.as_bytes();
        let refs = self.by_id.partition_point(|(base, _)| base.as_bytes() < id)
            ..self
                .by_id
                .partition_point(|(base, _)| base.as_bytes() <= id);
        (!ofs.is_empty() || !refs.is_empty()).then_some(Frame {
            entry: index,
            data: None,
            made: false,
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

    /// Counts an object made of a delta against what the pack may
    /// inflate to.
    fn count(&mut self, bytes: usize) -> Result<(), PackError> {
        self.inflated = (self.inflated.checked_add(bytes as u64))
            .filter(|total| *total <= self.limits.inflated_bytes)
            .ok_or(PackError::Limit("inflated bytes"))?;
        Ok(())
    }

    /// Resolves every delta below the plain entry `root`.
    fn below(&mut self, root: u32, kind: ObjectKind) -> Result<(), PackError> {
        let oid = self.oids[root as usize].expect("hashed by the scan");
        let Some(frame) = self.frame(root, &oid) else {
            return Ok(());
        };
        self.stack.push(frame);
        self.lowest = 0;
        while let Some(top) = self.stack.last_mut() {
            let next = (top.refs.next().map(|at| self.by_id[at].1))
                .or_else(|| top.ofs.next().map(|at| self.by_offset[at].1));
            let Some(child) = next else {
                let done = self.stack.pop().expect("the top");
                self.gone(done.data);
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
            // nothing more is made of the top frame's object: it stays
            // as a step on the way, without its bytes
            let top = self.stack.last_mut().expect("the top");
            if top.ofs.is_empty() && top.refs.is_empty() {
                let done = top.data.take();
                self.gone(done);
            }
            self.count(data.len())?;
            self.work.hashed += 1;
            let oid = ObjectId::hash_object(self.pack.algo(), kind, &data);
            self.oids[child as usize] = Some(oid);
            if let Some(mut frame) = self.frame(child, &oid) {
                self.held += data.len();
                frame.data = Some(Arc::new(Base::Held(data)));
                frame.made = true;
                self.stack.push(frame);
                self.let_go()?;
            }
        }
        Ok(())
    }

    /// Takes what a frame gave up out of the counts.
    fn gone(&mut self, data: Option<Arc<Base>>) {
        if let Some(data) = data {
            self.held -= data.held();
            self.spilled -= data.spilled();
        }
    }

    /// The object of the top frame. Where it was let go of it is made
    /// again from the nearest base below it that is there, or from the
    /// pack, and the bases made on the way are held anew within the
    /// limit: the nearest ones are those the next deltas need.
    fn base(&mut self) -> Result<Arc<Base>, PackError> {
        let top = self.stack.len() - 1;
        let from = (0..=top).rev().find(|at| self.stack[*at].data.is_some());
        let (mut data, from) = match from {
            Some(at) => (self.stack[at].data.clone().expect("found there"), at + 1),
            None => {
                self.inflate(self.stack[0].entry)?;
                (Arc::new(Base::Held(self.scratch.clone())), 0)
            }
        };
        for at in from..=top {
            if at > 0 {
                data = Arc::new(Base::Held(self.apply(&data, self.stack[at].entry)?));
            }
            if std::mem::replace(&mut self.stack[at].made, true) {
                self.count(data.len())?;
            }
            self.held += data.len();
            self.stack[at].data = Some(data.clone());
            self.lowest = self.lowest.min(at);
            self.let_go()?;
        }
        Ok(data)
    }

    /// Makes room while the bases held pass their limit, in those nearest
    /// the plain entry first: they are the last to be needed again. One
    /// that deltas still wait for goes to a file while there is room in
    /// those, and is otherwise let go of. The top frame keeps its own.
    fn let_go(&mut self) -> Result<(), PackError> {
        let top = self.stack.len() - 1;
        self.work.held = self.work.held.max(self.held);
        while self.held > self.limits.base_bytes && self.lowest < top {
            let frame = &mut self.stack[self.lowest];
            let waited_for = !frame.ofs.is_empty() || !frame.refs.is_empty();
            if let Some(data) = frame.data.take_if(|data| data.held() > 0) {
                self.held -= data.held();
                let room = self.limits.spill_bytes.saturating_sub(self.spilled);
                if waited_for && data.len() as u64 <= room {
                    self.spilled += data.len() as u64;
                    self.work.spilled = self.work.spilled.max(self.spilled);
                    frame.data = Some(Arc::new(data.spill()?));
                }
            }
            self.lowest += 1;
        }
        Ok(())
    }
}
