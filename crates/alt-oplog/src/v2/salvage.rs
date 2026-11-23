//! Frames that still read past where a log stops reading: what a cut
//! takes with it, and whether one of them says the log was synced beyond
//! that place.

use crate::v2::block::{BLOCK_HEADER_LEN, Layout};
use crate::v2::frame::{CLOSE_LEN, KIND_OP, View};
use crate::v2::open::op_of;
use crate::v2::scan::{Located, checkpoint_at, read_frame};
use crate::v2::stream::{ReadAt, walk_file};
use crate::{Op, OpLogError};

/// The frames that read in bytes of a log that follow no frame.
#[derive(Default)]
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

impl Salvaged {
    /// Takes in the frames that lie one before another up to where the
    /// frames of a block end: of the bytes `from..to` of the file, which
    /// are within one block.
    fn back(
        &mut self,
        read: &mut ReadAt,
        layout: &Layout,
        (from, to): (u64, u64),
        generation: u32,
    ) -> std::io::Result<()> {
        let data = read(from, (to - from) as usize)?;
        let filled = data
            .iter()
            .rposition(|byte| *byte != 0)
            .map_or(0, |at| at + 1);
        let frame_at = |end: usize| {
            let close = data.get(end.checked_sub(CLOSE_LEN)?..end)?;
            let len = u32::from_le_bytes(close[..4].try_into().unwrap()) as usize;
            let at = end.checked_sub(len)?;
            read_frame(layout, &data[at..end], generation).map(|view| (at, view))
        };
        // a checksum may end in zeros, which look like those that fill a block
        let ends = filled..=(filled + 4).min(data.len());
        let Some(mut end) = ends.into_iter().find(|end| frame_at(*end).is_some()) else {
            return Ok(());
        };
        let mut last = Vec::new();
        while let Some((at, view)) = frame_at(end) {
            let mut one = Salvaged::default();
            one.note(from + at as u64, from + end as u64, &view);
            last.push(one);
            end = at;
        }
        for one in last.into_iter().rev() {
            self.ops.extend(one.ops);
            self.covered += one.covered;
            self.claim = self.claim.max(one.claim);
            self.first = earlier(self.first, one.first);
        }
        Ok(())
    }
}

/// The checkpoint the block at `block` opens with, when one reads there
/// and ends within `limit`.
fn opening(
    read: &mut ReadAt,
    layout: &Layout,
    block: u64,
    limit: u64,
) -> std::io::Result<Option<Located>> {
    let header = layout.decode_header(&read(block, BLOCK_HEADER_LEN)?, block);
    match header {
        Some(header) if header.first as usize == BLOCK_HEADER_LEN => {
            checkpoint_at(layout, limit, block + BLOCK_HEADER_LEN as u64, read)
        }
        _ => Ok(None),
    }
}

/// Looks for frames in the file from byte `from`, where reading stopped
/// under `generation`, to byte `limit`: on from every block that opens
/// with a checkpoint, and back from where the frames end in the block a
/// walk stopped in and in the last one.
pub(crate) fn salvage(
    read: &mut ReadAt,
    layout: &Layout,
    from: u64,
    limit: u64,
    generation: u32,
) -> std::io::Result<Salvaged> {
    let mut found = Salvaged::default();
    let (mut at, mut generation) = (from, generation);
    while at < limit {
        let next = at.next_multiple_of(layout.block);
        if next > at {
            found.back(read, layout, (at, next.min(limit)), generation)?;
        }
        let mut block = next;
        let mut opened = None;
        while opened.is_none() && block + (BLOCK_HEADER_LEN as u64) < limit {
            opened = opening(read, layout, block, limit)?;
            block += layout.block;
        }
        let Some(Located { mut state, .. }) = opened else {
            let last = (limit - 1) / layout.block * layout.block;
            if next < limit {
                found.back(read, layout, (last.max(next), limit), generation)?;
            }
            break;
        };
        found.covered += state.end - state.last;
        found.claim = found.claim.max(state.synced);
        found.first = earlier(found.first, Some(state.last));
        let walked = walk_file(
            read,
            layout,
            &mut state,
            limit,
            false,
            &mut |at, end, view| {
                found.note(at, end, view);
                Ok(())
            },
        );
        // frames that do not follow one another end a walk as bytes do
        if let Err(OpLogError::Io(e)) = walked {
            return Err(e);
        }
        (at, generation) = (state.end, state.generation);
    }
    Ok(found)
}
