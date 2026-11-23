//! Reading frames out of bytes of a log, forward from a state.

use std::borrow::Cow;

use crate::OpLogError;
use crate::v2::block::{BLOCK_HEADER_LEN, BlockHeader, Layout};
use crate::v2::checkpoint::generation_of;
use crate::v2::frame::{
    HEADER_LEN, KIND_CHECKPOINT, KIND_OP, KIND_RECEIPT, SKIPPABLE, Seed, View, body_at, len_of,
    parse,
};
pub use crate::v2::locate::{Located, locate};
use crate::v2::state::State;
use crate::v2::verify::Full;

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
/// among them, which are given to `crossed`, and where it ends: `None`
/// where `data` does not hold as many bytes as its header says, laid out
/// as a frame's are.
pub(crate) fn lift_frame<'a>(
    layout: &Layout,
    data: &'a [u8],
    base: u64,
    at: u64,
    crossed: &mut dyn FnMut(&BlockHeader),
) -> Option<(Cow<'a, [u8]>, u64)> {
    let header = layout.lift_start(data, base, at, HEADER_LEN)?;
    let len = len_of(header.first_chunk()?)?;
    layout.lift_across(data, base, at, len, crossed)
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
        let skip = at.checked_sub(self.base);
        let skip = skip.and_then(|skip| usize::try_from(skip).ok());
        skip.and_then(|skip| self.data.get(skip..))
            .unwrap_or_default()
    }

    /// Where the frame after the one that ends at `at` starts, past the
    /// zeros that end a block and the header of the next one, with that
    /// header where it opens a block. `None` where those do not read.
    fn next_start(&self, mut at: u64) -> Option<(u64, Option<BlockHeader>)> {
        if !self.layout.opens_block(at) && self.from(at).first() == Some(&0) {
            let block_end = at + self.layout.room(at);
            let zeros = &self.from(at)[..(block_end.min(self.limit()) - at) as usize];
            if block_end >= self.limit() || zeros.iter().any(|byte| *byte != 0) {
                return None;
            }
            at = block_end;
        }
        if !self.layout.opens_block(at) {
            return Some((at, None));
        }
        let header = self.layout.decode_header(self.from(at), at);
        let header = header.filter(|header| header.first as usize == BLOCK_HEADER_LEN)?;
        Some((at + BLOCK_HEADER_LEN as u64, Some(header)))
    }

    /// The frame `bytes` are after the frames `state` stands for, read
    /// under the generation it is of, which `seed` is kept at: a
    /// checkpoint names the generation it and what follows are of.
    fn read<'a>(&self, bytes: &'a [u8], state: &State, seed: &mut (u32, Seed)) -> Option<View<'a>> {
        let generation = match bytes.first()? {
            &KIND_CHECKPOINT => generation_of(bytes.get(body_at(bytes.first_chunk()?)..)?)?,
            _ => state.generation,
        };
        if generation != seed.0 {
            *seed = (generation, Seed::new(&self.layout.id, generation));
        }
        parse(&seed.1, bytes)
    }

    /// Reads the frames that follow `state` and takes each into it, then
    /// hands it to `each` with where it starts and ends.
    pub fn run(
        &self,
        state: &mut State,
        each: &mut dyn FnMut(u64, u64, &View) -> Result<(), OpLogError>,
    ) -> Result<Stop, OpLogError> {
        let mut full = self.strict.then(|| Full::from(state));
        self.run_with(state, full.as_mut(), each)
    }

    /// [`Self::run`], as a part of the full check `full` when there is
    /// one, whatever `strict` says.
    pub(crate) fn run_with(
        &self,
        state: &mut State,
        mut full: Option<&mut Full>,
        each: &mut dyn FnMut(u64, u64, &View) -> Result<(), OpLogError>,
    ) -> Result<Stop, OpLogError> {
        let mut seed = (
            state.generation,
            Seed::new(&self.layout.id, state.generation),
        );
        while state.end < self.limit() {
            let Some((at, opened)) = self.next_start(state.end) else {
                return Ok(Stop::Unread);
            };
            let mut headers: Vec<BlockHeader> = opened.into_iter().collect();
            let mut crossed = |header: &BlockHeader| {
                if full.is_some() {
                    headers.push(*header);
                }
            };
            let lifted = lift_frame(&self.layout, self.data, self.base, at, &mut crossed);
            let Some((bytes, end)) = lifted else {
                return Ok(Stop::Unread);
            };
            let Some(view) = self.read(&bytes, state, &mut seed) else {
                return Ok(Stop::Unread);
            };
            let kind = view.head.kind;
            if opened.is_some() && kind != KIND_CHECKPOINT {
                return Ok(Stop::Unread);
            }
            let known = matches!(kind, KIND_OP | KIND_CHECKPOINT | KIND_RECEIPT);
            if !known && view.head.flags & SKIPPABLE == 0 {
                return Err(OpLogError::Format(
                    "the oplog holds a frame of a kind this alt does not know",
                ));
            }
            if let Some(full) = full.as_deref_mut() {
                full.frame(state, at, &view, seed.0, &headers)?;
            }
            if !state.take(at, end, &view)? {
                return Ok(Stop::Unread);
            }
            each(at, end, &view)?;
        }
        Ok(Stop::End)
    }
}
