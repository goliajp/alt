//! What the frames of a log come to, up to some frame: where the next one
//! goes and what it has to say of the ones before it.

use crate::v2::block::FILE_HEADER_LEN;
use crate::v2::checkpoint::{Checkpoint, Workspace, generation_of};
use crate::v2::frame::{KIND_CHECKPOINT, KIND_OP, OdbBounds, REQUIRED, Step, UNDOABLE, View};
use crate::{OpId, OpLogError, ROOT};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    pub generation: u32,
    /// where the last frame ends
    pub end: u64,
    /// where it starts, zero before the first
    pub last: u64,
    /// where the last checkpoint frame starts, or zero
    pub checkpoint: u64,
    pub seq: u64,
    pub head: OpId,
    pub head_at: u64,
    pub head_end: u64,
    /// the furthest a frame said the log was synced
    pub synced: u64,
    pub odb: OdbBounds,
    pub linked_at: u64,
    pub linked: u64,
    pub migrated: u64,
    /// where the last frame starts that says nothing is appended after it
    /// by whoever does not know it, or zero
    pub required: u64,
    pub workspaces: Vec<Workspace>,
}

impl State {
    /// A log that holds its header and nothing else.
    pub fn empty() -> Self {
        State {
            generation: 0,
            end: FILE_HEADER_LEN as u64,
            last: 0,
            checkpoint: 0,
            seq: 0,
            head: ROOT,
            head_at: 0,
            head_end: 0,
            synced: 0,
            odb: OdbBounds::default(),
            linked_at: 0,
            linked: 0,
            migrated: 0,
            required: 0,
            workspaces: Vec::new(),
        }
    }

    /// The log as the checkpoint frame at `at..end` says it stands.
    pub fn of(checkpoint: Checkpoint, at: u64, end: u64) -> Self {
        State {
            generation: checkpoint.generation,
            end,
            last: at,
            checkpoint: at,
            seq: checkpoint.seq,
            head: checkpoint.head(),
            head_at: checkpoint.head_at,
            head_end: checkpoint.head_end,
            synced: checkpoint.synced,
            odb: checkpoint.odb,
            linked_at: checkpoint.linked_at,
            linked: checkpoint.linked,
            migrated: checkpoint.migrated,
            required: checkpoint.required,
            workspaces: checkpoint.workspaces,
        }
    }

    /// The body of a checkpoint written now in `boot`, by a writer that
    /// knows the log synced up to `synced`.
    pub fn checkpoint(&self, boot: [u8; 16], synced: u64) -> Checkpoint {
        Checkpoint {
            boot,
            generation: self.generation,
            seq: self.seq,
            head: self.head.0,
            head_at: self.head_at,
            head_end: self.head_end,
            synced: self.synced.max(synced),
            odb: self.odb,
            linked_at: self.linked_at,
            linked: self.linked,
            migrated: self.migrated,
            required: self.required,
            workspaces: self.workspaces.clone(),
        }
    }

    pub fn workspace(&self, id: &[u8; 8]) -> Option<&Workspace> {
        let found = self.workspaces.binary_search_by(|row| row.id.cmp(id));
        found.ok().map(|index| &self.workspaces[index])
    }

    /// Whether the frame at `at` is the one that follows the frames
    /// before it: by where it says the last one starts, its count of ops
    /// and the ops it names as coming before it.
    fn follows(&self, at: u64, view: &View) -> bool {
        let head = &view.head;
        let op = head.kind == KIND_OP;
        let before = self.workspace(&head.workspace).map_or(0, |row| row.last);
        head.prev == self.last
            && Some(head.seq) == self.seq.checked_add(op as u64)
            && head.synced <= at
            && head.prev_in_workspace == if op { before } else { 0 }
            && view
                .ext
                .linked
                .is_none_or(|linked| linked == self.linked_at)
    }

    /// Takes the frame at `at..end` in, when it follows the frames before
    /// it. An op that follows them by everything but the parent its body
    /// names breaks the chain.
    pub fn take(&mut self, at: u64, end: u64, view: &View) -> Result<bool, OpLogError> {
        if !self.follows(at, view) {
            return Ok(false);
        }
        match view.head.kind {
            KIND_CHECKPOINT => {
                let said = generation_of(view.body);
                let Some(generation) = said.filter(|said| *said >= self.generation) else {
                    return Ok(false);
                };
                self.generation = generation;
                self.checkpoint = at;
            }
            KIND_OP => {
                let Some(op) = view.op.filter(|_| view.body.len() >= 32) else {
                    return Ok(false);
                };
                if view.body[..32] != self.head.0 {
                    return Err(OpLogError::Format("oplog chain broken"));
                }
                (self.seq, self.head) = (view.head.seq, op);
                (self.head_at, self.head_end) = (at, end);
                self.take_op(at, view);
            }
            _ => {}
        }
        (self.last, self.end) = (at, end);
        if view.head.flags & REQUIRED != 0 {
            self.required = at;
        }
        self.synced = self.synced.max(view.head.synced);
        if let Some(odb) = view.ext.odb {
            self.odb = odb;
        }
        Ok(true)
    }

    fn take_op(&mut self, at: u64, view: &View) {
        if view.ext.linked.is_some() {
            (self.linked_at, self.linked) = (at, self.linked.saturating_add(1));
        }
        let id = view.head.workspace;
        if id == [0; 8] {
            return;
        }
        let found = self.workspaces.binary_search_by(|row| row.id.cmp(&id));
        if view.ext.retired {
            if let Ok(index) = found {
                self.workspaces.remove(index);
            }
            return;
        }
        let index = found.unwrap_or_else(|index| {
            let row = Workspace {
                id,
                ..Workspace::default()
            };
            self.workspaces.insert(index, row);
            index
        });
        let row = &mut self.workspaces[index];
        row.last = at;
        if view.head.flags & UNDOABLE != 0 {
            row.undoable = at;
        }
        if let Some(sum) = view.ext.index_after {
            (row.index_at, row.index_sum) = (at, sum);
        }
        match view.ext.step {
            Some(Step::Begin) if row.step == 0 => row.step = at,
            Some(Step::Close) => row.step = 0,
            _ => {}
        }
    }
}
