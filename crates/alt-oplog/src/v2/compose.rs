//! The bytes a writer appends for a frame: the frame itself and, where it
//! does not fit into what is left of the block, the zeros that end the
//! block, the header of the next one and its checkpoint.

use crate::v2::block::{BLOCK_HEADER_LEN, BlockHeader, Layout};
use crate::v2::frame::{Cut, Ext, Head, KIND_CHECKPOINT, KIND_OP, Seed, View, encode, frame_len};
use crate::v2::state::State;
use crate::{OpId, OpLogError};

/// A frame to append.
#[derive(Debug, Clone, Default)]
pub struct New<'a> {
    pub kind: u8,
    pub flags: u8,
    pub workspace: [u8; 8],
    /// `linked` in it is filled in for a ref transaction
    pub ext: Ext,
    pub body: &'a [u8],
    pub op: Option<OpId>,
    /// a ref transaction
    pub linked: bool,
}

const TOO_MANY: OpLogError = OpLogError::Format("the oplog holds as many ops as it can count");

pub struct Composer {
    pub layout: Layout,
    /// the boot the checkpoints are written in
    pub boot: [u8; 16],
    /// up to where the writer knows the log synced
    pub synced: u64,
}

impl Composer {
    /// The bytes that append `new` to a log in `state`, to be written
    /// where that state ends; `state` then takes the frames among them in.
    pub fn frame(&self, state: &mut State, new: &New) -> Result<Vec<u8>, OpLogError> {
        let mut out = Vec::new();
        let len = encode(
            &Seed::new(&[0; 16], 0),
            &Head::default(),
            &self.ext(state, new),
            new.body,
            new.op.as_ref(),
        )?
        .len() as u64;
        if self.layout.opens_block(state.end) || len > self.layout.room(state.end) {
            self.open_block(state, None, &mut out)?;
        }
        let spans = len > self.layout.room(state.end);
        self.put(state, new, &mut out)?;
        if spans {
            // whoever reads the log from its end begins after the long frame
            self.put_checkpoint(state, None, &mut out)?;
        }
        Ok(out)
    }

    /// The bytes of a checkpoint appended on its own, which says what was
    /// cut off the log before it.
    pub fn checkpoint(&self, state: &mut State, cut: Option<Cut>) -> Result<Vec<u8>, OpLogError> {
        let mut out = Vec::new();
        self.put_checkpoint(state, cut, &mut out)?;
        Ok(out)
    }

    fn ext(&self, state: &State, new: &New) -> Ext {
        let mut ext = new.ext.clone();
        ext.linked = new.linked.then_some(state.linked_at);
        ext
    }

    fn put_checkpoint(
        &self,
        state: &mut State,
        cut: Option<Cut>,
        out: &mut Vec<u8>,
    ) -> Result<(), OpLogError> {
        let body = state.checkpoint(self.boot, self.synced).encode();
        let ext = Ext {
            cut,
            ..Ext::default()
        };
        let len = frame_len(ext.encode().len(), body.len(), false);
        if self.layout.opens_block(state.end) || len > self.layout.room(state.end) {
            return self.open_block(state, cut, out);
        }
        let new = New {
            kind: KIND_CHECKPOINT,
            ext,
            body: &body,
            ..New::default()
        };
        self.put(state, &new, out)
    }

    /// Ends the block `state` ends in and opens the next with its header
    /// and a checkpoint.
    fn open_block(
        &self,
        state: &mut State,
        cut: Option<Cut>,
        out: &mut Vec<u8>,
    ) -> Result<(), OpLogError> {
        let body = state.checkpoint(self.boot, self.synced).encode();
        if !self.layout.opens_block(state.end) {
            let room = self.layout.room(state.end);
            out.resize(out.len() + room as usize, 0);
            state.end += room;
        }
        out.extend_from_slice(&self.layout.encode_header(&BlockHeader {
            index: (state.end / self.layout.block) as u32,
            first: BLOCK_HEADER_LEN as u32,
            checkpoint: state.checkpoint,
            generation: state.generation,
        }));
        state.end += BLOCK_HEADER_LEN as u64;
        let ext = Ext {
            cut,
            ..Ext::default()
        };
        let new = New {
            kind: KIND_CHECKPOINT,
            ext,
            body: &body,
            ..New::default()
        };
        self.put(state, &new, out)
    }

    /// Lays the frame down where `state` ends and takes it into `state`.
    fn put(&self, state: &mut State, new: &New, out: &mut Vec<u8>) -> Result<(), OpLogError> {
        let at = state.end;
        let op = new.kind == KIND_OP;
        let before = state.workspace(&new.workspace).map_or(0, |row| row.last);
        let head = Head {
            kind: new.kind,
            flags: new.flags,
            prev: state.last,
            prev_in_workspace: if op { before } else { 0 },
            synced: state.synced.max(self.synced),
            workspace: new.workspace,
            seq: state.seq.checked_add(op as u64).ok_or(TOO_MANY)?,
        };
        let ext = self.ext(state, new);
        let seed = Seed::new(&self.layout.id, state.generation);
        let bytes = encode(&seed, &head, &ext, new.body, new.op.as_ref())?;
        let end = self.layout.end_of(at, bytes.len() as u64);
        let end = end.ok_or(OpLogError::TooLarge(bytes.len()))?;
        self.layout
            .lay(at, &bytes, state.checkpoint, state.generation, out);
        let view = View {
            head,
            ext,
            body: new.body,
            op: new.op,
        };
        match state.take(at, end, &view)? {
            true => Ok(()),
            false => Err(OpLogError::Format(
                "a frame does not follow the log it is appended to",
            )),
        }
    }
}
