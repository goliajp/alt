//! Frames that still read past where a log stops reading: what a cut
//! takes with it, and whether one of them says the log was synced beyond
//! that place.

use crate::Op;
use crate::v2::block::{BLOCK_HEADER_LEN, Layout};
use crate::v2::frame::{CLOSE_LEN, HEADER_LEN, KIND_OP, View};
use crate::v2::open::op_of;
use crate::v2::scan::{Located, Walk, checkpoint_at, read_frame};

/// The frames that read in bytes of a log that follow no frame: `rest`,
/// which begin at byte `base` of the file.
pub(crate) struct Salvaged {
    pub ops: Vec<Op>,
    /// how many of the bytes their frames take up
    pub covered: u64,
    /// the furthest one of them says the log was synced
    pub claim: u64,
    /// where the first of them starts
    pub first: Option<u64>,
}

fn earlier(one: Option<u64>, other: Option<u64>) -> Option<u64> {
    match (one, other) {
        (Some(one), Some(other)) => Some(one.min(other)),
        (one, other) => one.or(other),
    }
}

impl Salvaged {
    fn note(&mut self, at: u64, end: u64, view: &View) {
        if view.head.kind == KIND_OP
            && let Ok(op) = op_of(at, end, view)
        {
            self.ops.push(op);
        }
        self.covered += end - at;
        self.claim = self.claim.max(view.head.synced);
        self.first = earlier(self.first, Some(at));
    }
}

/// Looks for frames in `rest`: from every block in it that opens with a
/// checkpoint, and back from the end of it through the last block.
pub(crate) fn salvage(layout: &Layout, rest: &[u8], base: u64, generation: u32) -> Salvaged {
    let mut found = Salvaged {
        ops: Vec::new(),
        covered: 0,
        claim: 0,
        first: None,
    };
    let limit = base + rest.len() as u64;
    let mut read = |at: u64, len: usize| {
        let from = at.saturating_sub(base) as usize;
        Ok(rest.get(from..from + len).unwrap_or_default().to_vec())
    };
    let mut block = base.next_multiple_of(layout.block);
    let mut walked_to = base;
    while block + (BLOCK_HEADER_LEN as u64) < limit {
        let header = layout.decode_header(&rest[(block - base) as usize..], block);
        let opened = match header {
            Some(header) if header.first as usize == BLOCK_HEADER_LEN => {
                checkpoint_at(layout, limit, block + BLOCK_HEADER_LEN as u64, &mut read)
            }
            _ => Ok(None),
        };
        block += layout.block;
        let Ok(Some(Located { mut state, .. })) = opened else {
            continue;
        };
        found.covered += state.end - state.last;
        found.claim = found.claim.max(state.synced);
        found.first = earlier(found.first, Some(state.last));
        let walk = Walk {
            layout: *layout,
            data: rest,
            base,
            strict: false,
        };
        let _ = walk.run(&mut state, &mut |at, end, view| {
            found.note(at, end, view);
            Ok(())
        });
        walked_to = state.end;
        block = block.max(state.end.next_multiple_of(layout.block));
    }
    // the frames that lie one after another up to the end of the file
    let mut end = limit;
    let mut last = Vec::new();
    while walked_to < limit && end >= walked_to + (HEADER_LEN + CLOSE_LEN) as u64 {
        let close = &rest[(end - base) as usize - CLOSE_LEN..][..4];
        let len = u32::from_le_bytes(close.try_into().unwrap()) as u64;
        let Some(at) = end.checked_sub(len).filter(|at| *at >= walked_to) else {
            break;
        };
        if at / layout.block != (end - 1) / layout.block {
            break;
        }
        let bytes = &rest[(at - base) as usize..(end - base) as usize];
        let Some(view) = read_frame(layout, bytes, generation) else {
            break;
        };
        let mut one = Salvaged {
            ops: Vec::new(),
            covered: 0,
            claim: 0,
            first: None,
        };
        one.note(at, end, &view);
        last.push(one);
        end = at;
    }
    for one in last.into_iter().rev() {
        found.ops.extend(one.ops);
        found.covered += one.covered;
        found.claim = found.claim.max(one.claim);
        found.first = earlier(found.first, one.first);
    }
    found
}
