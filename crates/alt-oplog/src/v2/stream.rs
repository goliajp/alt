//! Walking the frames of the file itself, a stretch of it in memory at a
//! time: what a read holds is bounded by the stretch and by the longest
//! frame that is there, not by how long the file says it is.

use crate::OpLogError;
use crate::v2::block::{BLOCK_HEADER_LEN, Layout};
use crate::v2::frame::{CLOSE_LEN, HEADER_LEN, View, len_of};
use crate::v2::scan::{Stop, Walk};
use crate::v2::state::State;

/// Gives the bytes of the file at an offset, as many as asked for.
pub(crate) type ReadAt<'a> = dyn FnMut(u64, usize) -> std::io::Result<Vec<u8>> + 'a;

/// how much of the file a walk reads at once, unless a frame is longer
const STRETCH: u64 = 128 << 10;

/// Where a read that begins at `base`, where a frame ended, has to end
/// for the next frame to be read out of a file of `limit` bytes, as far
/// as `data` from there on tells, and how long the header of that frame
/// says it is once `data` holds it.
fn wanted(layout: &Layout, data: &[u8], base: u64, limit: u64) -> Option<(u64, Option<u64>)> {
    let mut at = base;
    if !layout.opens_block(at) && data.first() == Some(&0) {
        at = at.checked_add(layout.room(at))?;
    }
    if layout.opens_block(at) {
        at = at.checked_add(BLOCK_HEADER_LEN as u64)?;
    }
    let Some(header) = layout.lift_start(data, base, at, HEADER_LEN) else {
        let end = at.checked_add((HEADER_LEN + BLOCK_HEADER_LEN) as u64)?;
        return Some((end.min(limit), None));
    };
    let len = len_of(header.first_chunk()?)?;
    Some((layout.end_of(at, len)?, Some(len)))
}

/// Whether a frame of `len` bytes may end at `end`: by the length it
/// closes with, where a block header does not part that from the frame.
fn closes(read: &mut ReadAt, layout: &Layout, end: u64, len: u64) -> std::io::Result<bool> {
    let at = end - CLOSE_LEN as u64;
    let block = at / layout.block * layout.block;
    let parted = at / layout.block != (end - 1) / layout.block
        || (block != 0 && at - block < BLOCK_HEADER_LEN as u64);
    if parted {
        return Ok(true);
    }
    let said = read(at, 4)?;
    Ok(said
        .first_chunk()
        .map(|said| u32::from_le_bytes(*said) as u64)
        == Some(len))
}

/// Reads the frames that follow `state` in the file up to byte `limit`
/// of it, as [`Walk::run`] does those of bytes in memory.
pub(crate) fn walk_file(
    read: &mut ReadAt,
    layout: &Layout,
    state: &mut State,
    limit: u64,
    strict: bool,
    each: &mut dyn FnMut(u64, u64, &View) -> Result<(), OpLogError>,
) -> Result<Stop, OpLogError> {
    let mut reach = STRETCH;
    while state.end < limit {
        let base = state.end;
        let len = reach.min(limit - base);
        let data = read(base, len as usize)?;
        let walk = Walk {
            layout: *layout,
            data: &data,
            base,
            strict,
        };
        walk.run(state, each)?;
        if state.end > base {
            reach = STRETCH;
            continue;
        }
        // no frame was taken in: for want of bytes, or there is none
        let more = wanted(layout, &data, base, limit).filter(|(end, _)| *end > base + len);
        let Some((end, frame)) = more.filter(|(end, _)| *end <= limit) else {
            return Ok(Stop::Unread);
        };
        if let Some(frame) = frame
            && !closes(read, layout, end, frame)?
        {
            return Ok(Stop::Unread);
        }
        reach = end - base;
    }
    Ok(Stop::End)
}
