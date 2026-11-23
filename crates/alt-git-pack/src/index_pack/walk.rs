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
//! themselves.
//!
//! Several walks may go below different bases of one pack at once. What
//! they hold is one amount between them, and each keeps at least the
//! object it is working on.

use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;

use super::IndexWork;
use super::budget::Budget;
use super::deltas::Deltas;
use super::held::Base;
use crate::{PackError, delta};

/// The pack a walk goes through, and what is done with what it makes.
pub trait Objects {
    type Id: Ord + Copy;
    type Error: From<PackError>;

    /// What the entry inflates to: the object of a base that is no delta,
    /// the instructions of a delta. `scratch` is there to inflate into.
    fn payload<'a>(
        &'a mut self,
        entry: u32,
        scratch: &'a mut Vec<u8>,
    ) -> Result<&'a [u8], Self::Error>;

    /// Whether the delta was resolved before, through another entry of
    /// the object it is made of.
    fn reached(&self, delta: u32) -> bool;

    /// Takes the object a delta made, of the base `of` and `depth` deltas
    /// below a plain entry, and gives its id.
    fn made(
        &mut self,
        delta: u32,
        of: u32,
        depth: u32,
        object: &[u8],
    ) -> Result<Self::Id, Self::Error>;
}

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

pub struct Walk<'a, O: Objects> {
    pub objects: O,
    deltas: &'a Deltas<O::Id>,
    budget: &'a Budget,
    /// the streams inflated, deltas applied and objects made
    pub work: IndexWork,
    stack: Vec<Frame>,
    /// no frame below this one holds its base
    lowest: usize,
    scratch: Vec<u8>,
}

impl<'a, O: Objects> Walk<'a, O> {
    pub fn new(objects: O, deltas: &'a Deltas<O::Id>, budget: &'a Budget) -> Self {
        Walk {
            objects,
            deltas,
            budget,
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
            ofs,
            refs,
        })
    }

    fn apply(&mut self, base: &[u8], index: u32) -> Result<Vec<u8>, O::Error> {
        self.work.inflated += 1;
        let script = self.objects.payload(index, &mut self.scratch)?;
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
        let Some(frame) = self.frame(root, id) else {
            return Ok(());
        };
        self.stack.push(frame);
        self.lowest = 0;
        while let Some(top) = self.stack.last_mut() {
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
            let depth = depth.saturating_add(self.stack.len() as u32);
            if depth > self.budget.depth {
                return Err(PackError::Limit("delta depth").into());
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
        Ok(())
    }

    fn hold(&mut self, bytes: usize) {
        let held = self.budget.held.fetch_add(bytes, Relaxed) + bytes;
        self.budget.most_held.fetch_max(held, Relaxed);
    }

    /// Takes what a frame gave up out of the counts.
    fn gone(&mut self, data: Option<Arc<Base>>) {
        if let Some(data) = data {
            self.budget.held.fetch_sub(data.held(), Relaxed);
            self.budget.spilled.fetch_sub(data.spilled(), Relaxed);
        }
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
                let bytes = self.objects.payload(root, &mut self.scratch)?.to_vec();
                (Arc::new(Base::Held(bytes)), 0)
            }
        };
        for at in from..=top {
            if at > 0 {
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

    /// Makes room while the bases held pass their limit, in those nearest
    /// the plain entry first: they are the last to be needed again. One
    /// that deltas still wait for goes to a file while there is room in
    /// those, and is otherwise let go of. The top frame keeps its own.
    fn let_go(&mut self) -> Result<(), PackError> {
        let top = self.stack.len() - 1;
        let budget = self.budget;
        while budget.held.load(Relaxed) > budget.base_bytes && self.lowest < top {
            let frame = &mut self.stack[self.lowest];
            let waited_for = !frame.ofs.is_empty() || !frame.refs.is_empty();
            if let Some(data) = frame.data.take_if(|data| data.held() > 0) {
                budget.held.fetch_sub(data.held(), Relaxed);
                let bytes = data.len() as u64;
                let room = |spilled: u64| {
                    (spilled.checked_add(bytes)).filter(|total| *total <= budget.spill_bytes)
                };
                if waited_for && let Ok(before) = budget.spilled.try_update(Relaxed, Relaxed, room)
                {
                    budget.most_spilled.fetch_max(before + bytes, Relaxed);
                    frame.data = Some(Arc::new(data.spill()?));
                }
            }
            self.lowest += 1;
        }
        Ok(())
    }
}
