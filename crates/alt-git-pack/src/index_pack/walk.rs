//! Every delta below a base resolved to its object, from the base onwards.
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
//! themselves. The files of one pack are so many at the most, whatever
//! their bytes: a base beyond that is let go of as well.
//!
//! Several walks may go below different bases of one pack at once. What
//! they hold is one amount between them, and each keeps at least the
//! object it is working on. Where that object is a large one, the walk
//! has memory of its own for it: taken when the walk comes to it, and
//! kept for as long as the walk has a large object.

use std::sync::Arc;

use super::IndexWork;
use super::budget::Budget;
use super::deltas::Deltas;
use super::held::Base;
use super::memory::Taken;
use super::objects::Objects;
use crate::delta;

/// A base on the path from a plain entry to the delta being resolved.
pub(super) struct Frame {
    pub(super) entry: u32,
    /// let go of when the bases held pass their limit
    pub(super) data: Option<Arc<Base>>,
    /// whether its bytes were there once: inflating them again is counted
    made: bool,
    /// the object of the frame below it once more, by another entry
    same: bool,
    /// its deltas by offset and by id that are still to be resolved, as
    /// places in the sorted lists
    pub(super) ofs: std::ops::Range<usize>,
    pub(super) refs: std::ops::Range<usize>,
}

pub struct Walk<'a, O: Objects> {
    pub objects: O,
    deltas: &'a Deltas<O::Id>,
    pub(super) budget: &'a Budget<'a>,
    /// the memory of the object being worked on, where it is a large one
    pub(super) object: Option<Taken<'static>>,
    /// the streams inflated, deltas applied and objects made
    pub work: IndexWork,
    pub(super) stack: Vec<Frame>,
    /// no frame below this one holds its base
    pub(super) lowest: usize,
    scratch: Vec<u8>,
}

impl<'a, O: Objects> Walk<'a, O> {
    pub fn new(objects: O, deltas: &'a Deltas<O::Id>, budget: &'a Budget<'a>) -> Self {
        Walk {
            objects,
            deltas,
            budget,
            object: None,
            work: IndexWork::default(),
            stack: Vec::new(),
            lowest: 0,
            scratch: Vec::new(),
        }
    }

    /// A frame for the entry `index`, which is the object `id`, when any
    /// delta is made of it.
    fn frame(&self, index: u32, id: &O::Id) -> Option<Frame> {
        let (ofs, refs) = self.deltas.of(index, id);
        (!ofs.is_empty() || !refs.is_empty()).then_some(Frame {
            entry: index,
            data: None,
            made: false,
            same: false,
            ofs,
            refs,
        })
    }

    fn apply(&mut self, base: &[u8], index: u32) -> Result<Vec<u8>, O::Error> {
        self.work.inflated += 1;
        let script = self.objects.payload(index, &mut self.scratch)?;
        // the base, the instructions, and the result, whose buffer grows
        // to twice what it holds
        let made = delta::target_size(script)?.min(self.budget.object_bytes);
        let all = (base.len() as u64 + script.len() as u64).saturating_add(made.saturating_mul(2));
        if let Some(objects) = &self.budget.objects {
            objects.room(&mut self.object, all, self.budget.wait)?;
        }
        self.work.applied += 1;
        Ok(delta::apply_bounded(
            base,
            script,
            self.budget.object_bytes,
        )?)
    }

    /// Resolves every delta below `root`, which is no delta of this pack
    /// and is the object `id`, itself `depth` deltas below a plain entry.
    /// `root` need not be an entry: nothing is made of it by offset then.
    pub fn below(&mut self, root: u32, id: &O::Id, depth: u32) -> Result<(), O::Error> {
        self.below_made(&[root], root, id, depth)
    }

    /// [`Self::below`] for an object that deltas of this pack made, and
    /// for the entry `of` that is one of those that make it. `way` are
    /// entries that make it, from one that is no delta onwards, and
    /// `depth` is the deltas there are that way: the object is made again
    /// along it, and what is resolved is made of `of`.
    pub fn below_made(
        &mut self,
        way: &[u32],
        of: u32,
        id: &O::Id,
        depth: u32,
    ) -> Result<(), O::Error> {
        let Some(frame) = self.frame(of, id) else {
            return Ok(());
        };
        self.stack.extend(way.iter().map(|entry| Frame {
            entry: *entry,
            data: None,
            made: way.len() > 1,
            same: false,
            ofs: 0..0,
            refs: 0..0,
        }));
        match way.last() == Some(&of) {
            true => {
                let last = self.stack.last_mut().expect("the way");
                (last.ofs, last.refs) = (frame.ofs, frame.refs);
            }
            false => self.stack.push(Frame {
                same: true,
                ..frame
            }),
        }
        self.lowest = 0;
        let above = self.stack.len() - 1;
        while let Some(top) = self.stack.last_mut() {
            // the walk has no more than the base of the top frame now
            let has = top.data.as_ref().map_or(0, |data| data.held() as u64);
            if let Some(objects) = &self.budget.objects {
                objects.room(&mut self.object, has, self.budget.wait)?;
            }
            let next = (top.refs.next().map(|at| self.deltas.by_id[at].1))
                .or_else(|| top.ofs.next().map(|at| self.deltas.by_offset[at].1));
            let Some(child) = next else {
                let done = self.stack.pop().expect("the top");
                self.gone(done.data);
                self.lowest = self.lowest.min(self.stack.len());
                continue;
            };
            if self.objects.reached(child) {
                continue;
            }
            self.budget.waited_for()?;
            let depth = depth.saturating_add((self.stack.len() - above) as u32);
            if depth > self.budget.depth {
                let of = self.stack.last().expect("the top").entry;
                self.objects.beyond(child, of)?;
                continue;
            }
            let base = self.base()?;
            let data = self.apply(&base, child)?;
            drop(base);
            // nothing more is made of the top frame's object: it stays
            // as a step on the way, without its bytes
            let top = self.stack.last_mut().expect("the top");
            let of = top.entry;
            if top.ofs.is_empty() && top.refs.is_empty() {
                let done = top.data.take();
                self.gone(done);
            }
            self.budget.count(data.len())?;
            self.work.hashed += 1;
            let id = self.objects.made(child, of, depth, &data)?;
            if let Some(mut frame) = self.frame(child, &id) {
                self.hold(data.len());
                frame.data = Some(Arc::new(Base::Held(data)));
                frame.made = true;
                self.stack.push(frame);
                self.let_go()?;
            }
        }
        self.object = None;
        Ok(())
    }

    /// The object of the top frame. Where it was let go of it is made
    /// again from the nearest base below it that is there, or from the
    /// pack, and the bases made on the way are held anew within the
    /// limit: the nearest ones are those the next deltas need.
    fn base(&mut self) -> Result<Arc<Base>, O::Error> {
        let top = self.stack.len() - 1;
        let from = (0..=top).rev().find(|at| self.stack[*at].data.is_some());
        let (mut data, from) = match from {
            Some(at) => (self.stack[at].data.clone().expect("found there"), at + 1),
            None => {
                self.work.inflated += 1;
                let root = self.stack[0].entry;
                let bytes = self.objects.payload(root, &mut self.scratch)?;
                if let Some(objects) = &self.budget.objects {
                    objects.room(&mut self.object, bytes.len() as u64, self.budget.wait)?;
                }
                (Arc::new(Base::Held(bytes.to_vec())), 0)
            }
        };
        for at in from..=top {
            if at > 0 && !self.stack[at].same {
                data = Arc::new(Base::Held(self.apply(&data, self.stack[at].entry)?));
            }
            if std::mem::replace(&mut self.stack[at].made, true) {
                self.budget.count(data.len())?;
            }
            self.hold(data.len());
            self.stack[at].data = Some(data.clone());
            self.lowest = self.lowest.min(at);
            self.let_go()?;
        }
        Ok(data)
    }
}
