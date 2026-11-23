//! Reading the chain from somewhere other than where this handle began:
//! all of it, what follows an op a caller has kept an anchor of, and the
//! ref transactions between such an op and where the read began.

use std::borrow::Cow;

use crate::v2::block::{BLOCK_HEADER_LEN, FILE_HEADER_LEN};
use crate::v2::frame::{HEADER_LEN, KIND_OP, len_of};
use crate::v2::log::Log;
use crate::v2::open::op_of;
use crate::v2::scan::{Stop, Walk, read_frame};
use crate::v2::state::State;
use crate::{Anchor, Op, OpLogError};

impl Log {
    /// The op whose frame starts at `at`, with the frame it names as the
    /// ref transaction before it: `None` where no op frame reads there. A
    /// frame from before a cut reads under an earlier generation.
    fn op_at(&self, at: u64) -> Result<Option<(Op, Option<u64>)>, OpLogError> {
        let limit = self.state.end;
        let reach = (HEADER_LEN + BLOCK_HEADER_LEN).min(limit.saturating_sub(at) as usize);
        let start = self.source().at(at, reach)?;
        let header = self.layout.lift_start(&start, at, at, HEADER_LEN);
        let len = header.and_then(|header| len_of(header.first_chunk()?));
        let Some(end) = len.map(|len| self.layout.end_of(at, len)) else {
            return Ok(None);
        };
        if end > limit {
            return Ok(None);
        }
        let data = self.source().at(at, (end - at) as usize)?;
        let Some((bytes, _)) = self.layout.lift(&data, at, at, len.unwrap()) else {
            return Ok(None);
        };
        for generation in (0..=self.state.generation).rev() {
            if let Some(view) = read_frame(&self.layout, &bytes, generation) {
                let op = (view.head.kind == KIND_OP).then(|| op_of(at, end, &view));
                return Ok(op.transpose()?.map(|op| (op, view.ext.linked)));
            }
        }
        Ok(None)
    }

    /// Begins after `anchor` when the log holds the anchor's op where the
    /// anchor says: [`Self::recent`] is then the ops after it, as far as
    /// this handle read them, and of the ops between the anchor and where
    /// the read began the ref transactions. Otherwise every op is read.
    pub(super) fn begin_after(&mut self, anchor: Option<Anchor>) -> Result<(), OpLogError> {
        let Some(anchor) = anchor else {
            return Ok(());
        };
        let names = |op: &Op| {
            let frame = op.frame.unwrap();
            op.id == anchor.op && (frame.end, frame.seq) == (anchor.end, anchor.count)
        };
        let at = |op: &Op| op.frame.unwrap().at;
        if let Some(index) = self.ops.iter().position(|op| at(op) == anchor.start) {
            if names(&self.ops[index]) {
                (self.base, self.recent_from) = (Some(anchor), index + 1);
                return Ok(());
            }
        } else if anchor.end <= self.began
            && let Some((op, _)) = self.op_at(anchor.start)?
            && names(&op)
            && let Some(between) = self.linked_after(anchor.start)?
        {
            self.ops.splice(0..0, between);
            self.base = Some(anchor);
            return Ok(());
        }
        self.read_all()
    }

    /// The ref transactions whose frames start after `after` and before
    /// where the read began, in chain order, each found from the one
    /// after it; `None` where one of them does not read.
    fn linked_after(&self, after: u64) -> Result<Option<Vec<Op>>, OpLogError> {
        let mut between = Vec::new();
        let mut at = self.began_linked;
        while at > after {
            let Some((op, Some(before))) = self.op_at(at)? else {
                return Ok(None);
            };
            if before >= at {
                return Ok(None);
            }
            between.push(op);
            at = before;
        }
        between.reverse();
        Ok(Some(between))
    }

    /// Reads the log from its first frame to the last one held, as a full
    /// check does when `strict`, and gives its ops.
    fn read_through(&self, strict: bool) -> Result<Vec<Op>, OpLogError> {
        let data = self.source().at(0, self.state.end as usize)?;
        let walk = Walk {
            layout: self.layout,
            data: &data,
            base: 0,
            strict,
        };
        let mut state = State::empty();
        let mut ops = Vec::new();
        let stop = walk.run(&mut state, &mut |at, end, view| {
            if view.head.kind == KIND_OP {
                ops.push(op_of(at, end, view)?);
            }
            Ok(())
        })?;
        if stop != Stop::End || state != self.state {
            return Err(OpLogError::Format(
                "the oplog does not lead to where it ends",
            ));
        }
        Ok(ops)
    }

    pub fn ops(&self) -> Result<&[Op], OpLogError> {
        if self.holds_all() {
            return Ok(&self.ops);
        }
        if let Some(full) = self.full.get() {
            return Ok(full);
        }
        let all = self.read_through(false)?;
        Ok(self.full.get_or_init(|| all))
    }

    pub fn verify(&self) -> Result<(), OpLogError> {
        self.read_through(true).map(|_| ())
    }

    pub fn anchor_within(&self, bound: u64) -> Option<(Anchor, usize)> {
        if self.state.end <= bound {
            return self.anchor().map(|head| (head, 0));
        }
        let recent = self.recent();
        let began = self.base.filter(|base| base.end <= bound);
        let mut found = began.map(|base| (base, recent.len()));
        for (index, op) in recent.iter().enumerate() {
            let frame = op.frame.unwrap();
            if frame.end > bound {
                break;
            }
            let anchor = Anchor {
                op: op.id,
                start: frame.at,
                end: frame.end,
                count: frame.seq,
            };
            found = Some((anchor, recent.len() - index - 1));
        }
        found
    }

    pub fn since(&self, anchor: &Anchor) -> Result<Option<Cow<'_, [Op]>>, OpLogError> {
        let names = |op: &Op| {
            let frame = op.frame.unwrap();
            op.id == anchor.op
                && (frame.at, frame.end, frame.seq) == (anchor.start, anchor.end, anchor.count)
        };
        // the frames from where the read began on are all held
        let scanned = self
            .ops
            .iter()
            .position(|op| op.frame.unwrap().at >= self.began);
        let scanned = scanned.unwrap_or(self.ops.len());
        if let Some(index) = self.ops[scanned..].iter().position(names) {
            return Ok(Some(Cow::Borrowed(&self.ops[scanned + index + 1..])));
        }
        if anchor.start >= self.began.max(FILE_HEADER_LEN as u64) && !self.holds_all() {
            return Ok(None);
        }
        let all = self.ops()?;
        Ok(all
            .iter()
            .position(names)
            .map(|index| Cow::Borrowed(&all[index + 1..])))
    }
}
