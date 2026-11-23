//! Reading frames out of bytes of a log: forward from a state, and the
//! checkpoint to begin at.

use std::borrow::Cow;

use crate::OpLogError;
use crate::v2::block::{BLOCK_HEADER_LEN, Layout};
use crate::v2::checkpoint::{Checkpoint, generation_of};
use crate::v2::frame::{
    HEADER_LEN, KIND_CHECKPOINT, KIND_OP, KIND_RECEIPT, SKIPPABLE, Seed, View, body_at, len_of,
    parse,
};
use crate::v2::state::State;

/// Why a walk stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    /// the last frame ends where the bytes do
    End,
    /// what follows the last frame is no frame that follows it
    Unread,
}

/// Bytes of a log, `data`, that begin at byte `base` of the file.
pub struct Walk<'a> {
    pub layout: Layout,
    pub data: &'a [u8],
    pub base: u64,
    /// also derive what every checkpoint and block header says and hash
    /// every op: what a full check does
    pub strict: bool,
}

/// The bytes of the frame that starts at `at`, without the block headers
/// among them, and where it ends: `None` where `data` does not hold as
/// many bytes as its header says, laid out as a frame's are.
fn lift_frame<'a>(
    layout: &Layout,
    data: &'a [u8],
    base: u64,
    at: u64,
) -> Option<(Cow<'a, [u8]>, u64)> {
    let (header, _) = layout.lift(data, base, at, HEADER_LEN as u64)?;
    layout.lift(data, base, at, len_of(header.first_chunk()?)?)
}

/// The frame `bytes` are when they read: a checkpoint under the
/// generation it names itself, any other under `generation`.
pub(crate) fn read_frame<'a>(
    layout: &Layout,
    bytes: &'a [u8],
    generation: u32,
) -> Option<View<'a>> {
    let header: &[u8; HEADER_LEN] = bytes.first_chunk()?;
    let generation = match header[0] {
        KIND_CHECKPOINT => generation_of(bytes.get(body_at(header)..)?)?,
        _ => generation,
    };
    parse(&Seed::new(&layout.id, generation), bytes)
}

impl Walk<'_> {
    fn limit(&self) -> u64 {
        self.base + self.data.len() as u64
    }

    fn from(&self, at: u64) -> &[u8] {
        &self.data[(at - self.base) as usize..]
    }

    /// Where the frame after the one that ends at `at` starts, past the
    /// zeros that end a block and the header of the next one; whether it
    /// opens a block. `None` where those do not read.
    fn next_start(&self, state: &State, mut at: u64) -> Result<Option<(u64, bool)>, OpLogError> {
        if !self.layout.opens_block(at) && self.from(at)[0] == 0 {
            let block_end = at + self.layout.room(at);
            let zeros = &self.from(at)[..(block_end.min(self.limit()) - at) as usize];
            if block_end >= self.limit() || zeros.iter().any(|byte| *byte != 0) {
                return Ok(None);
            }
            at = block_end;
        }
        if !self.layout.opens_block(at) {
            return Ok(Some((at, false)));
        }
        let header = self.layout.decode_header(self.from(at), at);
        let Some(header) = header.filter(|header| header.first as usize == BLOCK_HEADER_LEN) else {
            return Ok(None);
        };
        if self.strict && header.checkpoint != state.checkpoint {
            return Err(OpLogError::Format(
                "an oplog block names another checkpoint than the one before it",
            ));
        }
        Ok(Some((at + BLOCK_HEADER_LEN as u64, true)))
    }

    /// What a full check holds a frame to beyond its checksum.
    fn verify(&self, state: &State, view: &View) -> Result<(), OpLogError> {
        match view.head.kind {
            KIND_OP => {
                let id = alt_hash::blake3::hash(view.body);
                if view.op.is_none_or(|op| op.0 != *id.as_bytes()) {
                    return Err(OpLogError::Format("an op's id is not the hash of its body"));
                }
            }
            KIND_CHECKPOINT => {
                let said = Checkpoint::decode(view.body);
                // the boot and the generation are the checkpoint's own to say
                let derived = said.as_ref().map(|said| Checkpoint {
                    generation: said.generation,
                    ..state.checkpoint(said.boot, view.head.synced)
                });
                if said.is_none() || said != derived {
                    return Err(OpLogError::Format(
                        "an oplog checkpoint disagrees with the frames before it",
                    ));
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Reads the frames that follow `state` and takes each into it, then
    /// hands it to `each` with where it starts and ends.
    pub fn run(
        &self,
        state: &mut State,
        each: &mut dyn FnMut(u64, u64, &View) -> Result<(), OpLogError>,
    ) -> Result<Stop, OpLogError> {
        let mut seed = (
            state.generation,
            Seed::new(&self.layout.id, state.generation),
        );
        while state.end < self.limit() {
            let Some((at, opens)) = self.next_start(state, state.end)? else {
                return Ok(Stop::Unread);
            };
            let Some((bytes, end)) = lift_frame(&self.layout, self.data, self.base, at) else {
                return Ok(Stop::Unread);
            };
            // a checkpoint names the generation it and what follows are of
            let generation = match bytes[0] {
                KIND_CHECKPOINT => generation_of(&bytes[body_at(bytes.first_chunk().unwrap())..]),
                _ => Some(state.generation),
            };
            if generation != Some(seed.0) {
                let Some(generation) = generation else {
                    return Ok(Stop::Unread);
                };
                seed = (generation, Seed::new(&self.layout.id, generation));
            }
            let Some(view) = parse(&seed.1, &bytes) else {
                return Ok(Stop::Unread);
            };
            let kind = view.head.kind;
            if opens && kind != KIND_CHECKPOINT {
                return Ok(Stop::Unread);
            }
            let known = matches!(kind, KIND_OP | KIND_CHECKPOINT | KIND_RECEIPT);
            if !known && view.head.flags & SKIPPABLE == 0 {
                return Err(OpLogError::Format(
                    "the oplog holds a frame of a kind this alt does not know",
                ));
            }
            if self.strict {
                self.verify(state, &view)?;
            }
            if !state.take(at, end, &view)? {
                return Ok(Stop::Unread);
            }
            each(at, end, &view)?;
        }
        Ok(Stop::End)
    }
}

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
    read: &mut dyn FnMut(u64, usize) -> std::io::Result<Vec<u8>>,
) -> std::io::Result<Option<Located>> {
    if at + HEADER_LEN as u64 > len {
        return Ok(None);
    }
    // a header that runs into the next block has a block header in it
    let reach = (HEADER_LEN + BLOCK_HEADER_LEN).min(len.saturating_sub(at) as usize);
    let start = read(at, reach)?;
    let Some((header, _)) = layout.lift(&start, at, at, HEADER_LEN as u64) else {
        return Ok(None);
    };
    let header: &[u8; HEADER_LEN] = header.first_chunk().unwrap();
    let end = match len_of(header) {
        Some(frame) if header[0] == KIND_CHECKPOINT => layout.end_of(at, frame),
        _ => return Ok(None),
    };
    if end > len {
        return Ok(None);
    }
    let data = read(at, (end - at) as usize)?;
    let read = lift_frame(layout, &data, at, at);
    let Some((bytes, _)) = read.filter(|(bytes, _)| read_frame(layout, bytes, 0).is_some()) else {
        return Ok(None);
    };
    let body = &bytes[body_at(header)..bytes.len() - 8];
    Ok(Checkpoint::decode(body).map(|said| Located {
        boot: said.boot,
        state: State::of(said, at, end),
    }))
}

/// Where a read of a log `len` bytes long begins: the state of the
/// last checkpoint a block header leads to which reads and ends within
/// `len`, or of the empty log where there is none. `read` gives bytes of
/// the file.
pub fn locate(
    layout: &Layout,
    len: u64,
    read: &mut dyn FnMut(u64, usize) -> std::io::Result<Vec<u8>>,
) -> std::io::Result<Located> {
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
                at => {
                    if let Some(state) = checkpoint_at(layout, len, at, read)? {
                        return Ok(state);
                    }
                }
            }
        }
        block -= 1;
    }
    Ok(Located::empty())
}
