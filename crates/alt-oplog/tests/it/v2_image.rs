//! A version 2 log built in memory, frame by frame, as a writer lays it.

use alt_oplog::v2::block::{FileHeader, Layout};
use alt_oplog::v2::compose::{Composer, New};
use alt_oplog::v2::frame::{Ext, KIND_OP, KIND_RECEIPT, View};
use alt_oplog::v2::scan::{Stop, Walk};
use alt_oplog::v2::state::State;
use alt_oplog::{OpId, OpLogError};

pub struct Image {
    pub composer: Composer,
    pub state: State,
    pub bytes: Vec<u8>,
    /// where each frame starts and ends, and its kind, in the order laid
    pub frames: Vec<(u64, u64, u8)>,
}

pub fn header(block_exp: u8) -> FileHeader {
    FileHeader {
        block_exp,
        incompatible: 0,
        read_compatible: 0,
        compatible: 0,
        id: *b"an oplog's name.",
        created_ms: 1_700_000_000_000,
    }
}

impl Image {
    pub fn new(block_exp: u8) -> Self {
        let header = header(block_exp);
        Image {
            composer: Composer {
                layout: header.layout(),
                boot: [7; 16],
                synced: 0,
            },
            state: State::empty(),
            bytes: header.encode().to_vec(),
            frames: Vec::new(),
        }
    }

    pub fn layout(&self) -> Layout {
        self.composer.layout
    }

    /// Notes the frames `added` holds, which were laid from `from` on.
    fn note(&mut self, from: State, added: &[u8]) {
        let mut state = from;
        let mut data = self.bytes.clone();
        data.extend_from_slice(added);
        let walk = Walk {
            layout: self.layout(),
            data: &data,
            base: 0,
            strict: true,
        };
        let frames = &mut self.frames;
        let stop = walk.run(&mut state, &mut |at, end, view| {
            frames.push((at, end, view.head.kind));
            Ok(())
        });
        assert_eq!(stop.unwrap(), Stop::End);
        assert_eq!(state, self.state, "the writer and a reader agree");
        self.bytes = data;
    }

    pub fn append(&mut self, new: &New) {
        let from = self.state.clone();
        let added = self.composer.frame(&mut self.state, new).unwrap();
        self.note(from, &added);
    }

    /// An op with `payload`, of `workspace` when that is not zeros.
    pub fn op(&mut self, workspace: u8, payload: &[u8]) -> OpId {
        self.op_with(workspace, payload, Ext::default(), false)
    }

    pub fn op_with(&mut self, workspace: u8, payload: &[u8], ext: Ext, linked: bool) -> OpId {
        let body = alt_oplog::v2::op_body(&self.state.head, self.state.seq, "actor", payload);
        let id = OpId(*alt_hash::blake3::hash(&body).as_bytes());
        self.append(&New {
            kind: KIND_OP,
            flags: 0,
            workspace: [workspace; 8],
            ext,
            body: &body,
            op: Some(id),
            linked,
        });
        id
    }

    pub fn receipt(&mut self, body: &[u8]) {
        self.append(&New {
            kind: KIND_RECEIPT,
            body,
            ..New::default()
        });
    }

    /// How many bytes an op frame takes with a payload of `payload` bytes.
    pub fn op_len(payload: usize) -> u64 {
        // header, the fixed part of a body, the actor, id, close
        (48 + 78 + 5 + payload + 32 + 8) as u64
    }
}

/// Where each frame starts and ends with its kind, the state they come
/// to, and how the walk stopped.
pub type Walked = (Vec<(u64, u64, u8)>, State, Stop);

/// The frames a walk over `data` takes in from the empty log, and how it
/// stopped.
pub fn walked(layout: Layout, data: &[u8], strict: bool) -> Result<Walked, OpLogError> {
    let mut state = State::empty();
    let mut frames = Vec::new();
    let walk = Walk {
        layout,
        data,
        base: 0,
        strict,
    };
    let stop = walk.run(&mut state, &mut |at: u64, end: u64, view: &View| {
        frames.push((at, end, view.head.kind));
        Ok(())
    })?;
    Ok((frames, state, stop))
}
