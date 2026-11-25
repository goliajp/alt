//! What a full check holds a frame to beyond what every read does. The
//! id of an op is the hash of its body. A checkpoint says what the frames
//! before it come to. A receipt reads, and says of its request what the
//! frames of that request say. The header of a block the frame begins or
//! runs across names the checkpoint before it and is of the frame's
//! generation.

use std::collections::BTreeMap;

use crate::OpLogError;
use crate::v2::block::{BlockHeader, FILE_HEADER_LEN};
use crate::v2::checkpoint::Checkpoint;
use crate::v2::frame::{KIND_CHECKPOINT, KIND_OP, KIND_RECEIPT, View};
use crate::v2::receipt::Receipt;
use crate::v2::state::State;

/// What a full check keeps from frame to frame.
pub(crate) struct Full {
    /// the check began at the first frame of the log
    whole: bool,
    /// where the first op of a request without a receipt yet starts, and
    /// how many ops it has
    requests: BTreeMap<[u8; 16], (u64, u32)>,
}

/// Holds the headers of the blocks a frame of `generation` begins or runs
/// across, after the frames `state` stands for, to what they say of it.
fn blocks(state: &State, generation: u32, headers: &[BlockHeader]) -> Result<(), OpLogError> {
    let named = |header: &BlockHeader| header.checkpoint == state.checkpoint;
    if !headers.iter().all(named) {
        return Err(OpLogError::Format(
            "an oplog block names another checkpoint than the one before it",
        ));
    }
    if headers.iter().any(|header| header.generation != generation) {
        return Err(OpLogError::Format(
            "an oplog block is of another generation than the frame in it",
        ));
    }
    Ok(())
}

impl Full {
    /// For a check of the frames that follow `state`.
    pub fn from(state: &State) -> Self {
        Full {
            whole: state.end == FILE_HEADER_LEN as u64,
            requests: BTreeMap::new(),
        }
    }

    /// Holds the frame at `at`, which is of `generation` and comes after
    /// the frames `state` stands for, to all of it; `headers` are those
    /// of the blocks it begins or runs across.
    pub fn frame(
        &mut self,
        state: &State,
        at: u64,
        view: &View,
        generation: u32,
        headers: &[BlockHeader],
    ) -> Result<(), OpLogError> {
        blocks(state, generation, headers)?;
        match view.head.kind {
            KIND_OP => {
                let id = alt_hash::blake3::hash(view.body);
                if view.op.is_none_or(|op| op.0 != *id.as_bytes()) {
                    return Err(OpLogError::Format("an op's id is not the hash of its body"));
                }
                if let Some(request) = view.ext.request {
                    self.requests.entry(request).or_insert((at, 0)).1 += 1;
                }
            }
            KIND_CHECKPOINT => {
                let said = Checkpoint::decode(view.body);
                // the boot and the generation are the checkpoint's own to
                // say, as is whether it holds the whole table
                let derived = said.as_ref().map(|said| Checkpoint {
                    generation: said.generation,
                    ..state.checkpoint(said.boot, view.head.synced, said.base == 0)
                });
                // a cut names the one before it and what was written since
                let before = (state.cut, state.generation);
                let cut = view.ext.cut.map(|cut| (cut.after, cut.generation));
                if said.is_none() || said != derived || cut.is_some_and(|cut| cut != before) {
                    return Err(OpLogError::Format(
                        "an oplog checkpoint disagrees with the frames before it",
                    ));
                }
            }
            KIND_RECEIPT => self.receipt(view)?,
            _ => {}
        }
        Ok(())
    }

    fn receipt(&mut self, view: &View) -> Result<(), OpLogError> {
        let said = Receipt::decode(view.body);
        let Some(said) = said.filter(|said| view.ext.request == Some(said.request)) else {
            return Err(OpLogError::Format("an oplog receipt does not read"));
        };
        let ops = self.requests.remove(&said.request).unwrap_or_default();
        if self.whole && ops != (said.first_op, said.ops) {
            return Err(OpLogError::Format(
                "an oplog receipt disagrees with the frames of its request",
            ));
        }
        Ok(())
    }
}
