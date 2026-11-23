//! The checkpoint a read begins at.

use crate::v2::block::{BLOCK_HEADER_LEN, Layout};
use crate::v2::checkpoint::Checkpoint;
use crate::v2::frame::{HEADER_LEN, KIND_CHECKPOINT, body_at, len_of};
use crate::v2::scan::{lift_frame, read_frame};
use crate::v2::state::State;
use crate::v2::stream::{ReadAt, read_claimed};

/// Where a read begins: the log as a checkpoint says it stands, or the
/// empty log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Located {
    pub state: State,
    /// the boot the checkpoint was written in, zeros for the empty log
    pub boot: [u8; 16],
}

impl Located {
    pub(crate) fn empty() -> Self {
        Located {
            state: State::empty(),
            boot: [0; 16],
        }
    }
}

/// What the checkpoint frame at `at` says, and where it ends, when one
/// reads there and ends within `len`.
fn said_at(
    layout: &Layout,
    len: u64,
    at: u64,
    read: &mut ReadAt,
) -> std::io::Result<Option<(Checkpoint, u64)>> {
    let Some(left) = len
        .checked_sub(at)
        .filter(|left| *left >= HEADER_LEN as u64)
    else {
        return Ok(None);
    };
    // a header that runs into the next block has a block header in it
    let reach = left.min((HEADER_LEN + BLOCK_HEADER_LEN) as u64);
    let start = read(at, reach as usize)?;
    let Some(header) = layout.lift_start(&start, at, at, HEADER_LEN) else {
        return Ok(None);
    };
    let header: &[u8; HEADER_LEN] = header.first_chunk().unwrap();
    let frame = len_of(header).filter(|_| header[0] == KIND_CHECKPOINT);
    let end = frame.and_then(|frame| layout.end_of(at, frame));
    let Some((frame, end)) = frame.zip(end).filter(|(_, end)| *end <= len) else {
        return Ok(None);
    };
    let Some(data) = read_claimed(read, layout, (at, end), frame)? else {
        return Ok(None);
    };
    let read = lift_frame(layout, &data, at, at, &mut |_| {});
    let Some((bytes, _)) = read.filter(|(bytes, _)| read_frame(layout, bytes, 0).is_some()) else {
        return Ok(None);
    };
    let body = &bytes[body_at(header)..bytes.len() - 8];
    Ok(Checkpoint::decode(body).map(|said| (said, end)))
}

/// The state the checkpoint frame at `at` gives, when one reads there
/// and ends within `len`: with the table of the checkpoint it names,
/// which lies before it, where it holds the rows that changed since.
pub(crate) fn checkpoint_at(
    layout: &Layout,
    len: u64,
    at: u64,
    read: &mut ReadAt,
) -> std::io::Result<Option<Located>> {
    let Some((said, end)) = said_at(layout, len, at, read)? else {
        return Ok(None);
    };
    let (boot, mut changed) = (said.boot, Vec::new());
    let whole = match said.base {
        0 => Some(said),
        base if base < at => {
            changed = said.workspaces.iter().map(|row| row.id).collect();
            let table = said_at(layout, at, base, read)?;
            table.and_then(|(table, _)| said.over(table))
        }
        _ => None,
    };
    Ok(whole
        .filter(|whole| whole.precedes(at))
        .map(|whole| Located {
            boot,
            state: State::of(whole, changed, at, end),
        }))
}

/// Gives the stretches of the first bytes of the file, as many as it is
/// told, that hold data, in their order.
pub(crate) type Stretches<'a> = dyn FnMut(u64) -> std::io::Result<Vec<(u64, u64)>> + 'a;

/// The last block before block number `block` whose header lies, in part
/// at least, in one of the stretches `held` of the file that hold data;
/// zero when there is none.
fn block_before(layout: &Layout, held: &[(u64, u64)], block: u64) -> u64 {
    let reached = |(start, end): &(u64, u64)| {
        let last = ((end - 1) / layout.block).min(block - 1);
        (last * layout.block + BLOCK_HEADER_LEN as u64 > *start).then_some(last)
    };
    held.iter().rev().find_map(reached).unwrap_or(0)
}

/// Where a read of a log `len` bytes long begins: the state of the
/// last checkpoint a block header leads to which reads and ends within
/// `len`, or of the empty log where there is none. `read` gives bytes of
/// the file.
pub fn locate(layout: &Layout, len: u64, read: &mut ReadAt) -> std::io::Result<Located> {
    locate_past(layout, len, read, &mut |len| Ok(vec![(0, len)]))
}

/// [`locate`], with the blocks passed over whose headers lie in holes of
/// the file: `data` is asked where the file holds data once a block is
/// found to open with no header, and those of the blocks before it that
/// lie in a hole are not read to find the same of them.
pub(crate) fn locate_past(
    layout: &Layout,
    len: u64,
    read: &mut ReadAt,
    data: &mut Stretches,
) -> std::io::Result<Located> {
    let mut block = len.saturating_sub(1) / layout.block;
    let mut held = None;
    while block > 0 {
        let start = block * layout.block;
        let header = match len - start >= BLOCK_HEADER_LEN as u64 {
            true => layout.decode_header(&read(start, BLOCK_HEADER_LEN)?, start),
            false => None,
        };
        let Some(header) = header else {
            let held = match &held {
                Some(held) => held,
                None => held.insert(data(len)?),
            };
            block = block_before(layout, held, block);
            continue;
        };
        if header.first != 0
            && let Some(state) = checkpoint_at(layout, len, start + header.first as u64, read)?
        {
            return Ok(state);
        }
        match header.checkpoint {
            0 => return Ok(Located::empty()),
            // the one it names lies before it
            at if at < start => {
                if let Some(state) = checkpoint_at(layout, len, at, read)? {
                    return Ok(state);
                }
            }
            _ => {}
        }
        block -= 1;
    }
    Ok(Located::empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_whose_headers_lie_in_holes_are_passed_over() {
        let layout = Layout {
            block: 4096,
            id: [0; 16],
        };
        let before = |held: &[(u64, u64)], block| block_before(&layout, held, block);
        // without holes, the block before
        assert_eq!(before(&[(0, 1 << 30)], 500), 499);
        assert_eq!(before(&[(0, 1 << 30)], 1), 0);
        // a hole from within block 2 on: its header is the last
        assert_eq!(before(&[(0, 9000)], 262_144), 2);
        // data that ends where a block starts reaches no header of it
        assert_eq!(before(&[(0, 8192)], 262_144), 1);
        // a stretch between two headers holds none: the one before does
        assert_eq!(before(&[(0, 5000), (40_000, 40_900)], 90), 1);
        // one that begins within a header holds a part of it
        assert_eq!(before(&[(0, 5000), (40_960 + 31, 50_000)], 90), 12);
        // one that begins where a header ends holds the headers after it
        assert_eq!(before(&[(0, 5000), (40_960 + 32, 45_000)], 90), 1);
        assert_eq!(before(&[(0, 5000), (40_960 + 32, 50_000)], 90), 12);
        assert_eq!(before(&[(0, 5000), (40_960 + 32, 50_000)], 12), 11);
        assert_eq!(before(&[(0, 5000), (40_960 + 32, 50_000)], 11), 1);
        assert_eq!(before(&[], 90), 0);
    }
}
