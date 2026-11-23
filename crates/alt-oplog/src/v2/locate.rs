//! The checkpoint a read begins at.

use crate::v2::block::{BLOCK_HEADER_LEN, Layout};
use crate::v2::checkpoint::Checkpoint;
use crate::v2::frame::{HEADER_LEN, KIND_CHECKPOINT, body_at, len_of};
use crate::v2::scan::{lift_frame, read_frame};
use crate::v2::state::State;
use crate::v2::stream::ReadAt;

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

/// The state the checkpoint frame at `at` gives, when one reads there
/// and ends within `len`.
pub(crate) fn checkpoint_at(
    layout: &Layout,
    len: u64,
    at: u64,
    read: &mut ReadAt,
) -> std::io::Result<Option<Located>> {
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
    let end = match len_of(header) {
        Some(frame) if header[0] == KIND_CHECKPOINT => layout.end_of(at, frame),
        _ => None,
    };
    let Some(end) = end.filter(|end| *end <= len) else {
        return Ok(None);
    };
    let data = read(at, (end - at) as usize)?;
    let read = lift_frame(layout, &data, at, at, &mut |_| {});
    let Some((bytes, _)) = read.filter(|(bytes, _)| read_frame(layout, bytes, 0).is_some()) else {
        return Ok(None);
    };
    let body = &bytes[body_at(header)..bytes.len() - 8];
    let said = Checkpoint::decode(body).filter(|said| said.precedes(at));
    Ok(said.map(|said| Located {
        boot: said.boot,
        state: State::of(said, at, end),
    }))
}

/// Where a read of a log `len` bytes long begins: the state of the
/// last checkpoint a block header leads to which reads and ends within
/// `len`, or of the empty log where there is none. `read` gives bytes of
/// the file.
pub fn locate(layout: &Layout, len: u64, read: &mut ReadAt) -> std::io::Result<Located> {
    let mut block = len.saturating_sub(1) / layout.block;
    while block > 0 {
        let start = block * layout.block;
        let header = match len - start >= BLOCK_HEADER_LEN as u64 {
            true => layout.decode_header(&read(start, BLOCK_HEADER_LEN)?, start),
            false => None,
        };
        if let Some(header) = header {
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
        }
        block -= 1;
    }
    Ok(Located::empty())
}
