//! The body of a checkpoint frame: what the frames before it come to, so
//! that a reader need not read them.
//!
//! ```text
//! [version u8, 1][the boot it was written in 16][generation u32]
//! [ops so far u64][the last op's id 32][where its frame starts u64][ends u64]
//! [up to where the log is synced u64]
//! [the object store's four synced lengths 4 x u64][its generation u32]
//! [the last ref transaction's frame u64][ref transactions so far u64]
//! [before where the ops may be of the kinds a version 1 log held u64]
//! [workspaces u32], each
//!   [id 8][its last op's frame u64][its last op that can be undone u64]
//!   [its last op that changed the index u64][the index's checksum then 20]
//!   [the frame that began the step it is in u64, or zero]
//! ```
//!
//! Everything in it follows from the frames before it, and a full check
//! derives it again.

use crate::OpId;
use crate::v2::frame::OdbBounds;

const VERSION: u8 = 1;
const FIXED_LEN: usize = 1 + 16 + 4 + 8 + 32 + 8 + 8 + 8 + 36 + 8 + 8 + 8 + 4;
const WORKSPACE_LEN: usize = 8 + 8 + 8 + 8 + 20 + 8;
/// where the generation lies in the body
const GENERATION_AT: usize = 17;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Workspace {
    pub id: [u8; 8],
    pub last: u64,
    pub undoable: u64,
    pub index_at: u64,
    pub index_sum: [u8; 20],
    pub step: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Checkpoint {
    pub boot: [u8; 16],
    pub generation: u32,
    pub seq: u64,
    pub head: [u8; 32],
    pub head_at: u64,
    pub head_end: u64,
    pub synced: u64,
    pub odb: OdbBounds,
    pub linked_at: u64,
    pub linked: u64,
    pub migrated: u64,
    /// in the order of their ids
    pub workspaces: Vec<Workspace>,
}

/// The generation the checkpoint with this body is written under, which
/// its own checksum begins from.
pub fn generation_of(body: &[u8]) -> Option<u32> {
    let bytes = body.get(GENERATION_AT..GENERATION_AT + 4)?;
    Some(u32::from_le_bytes(bytes.try_into().unwrap()))
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take<const N: usize>(&mut self) -> Option<[u8; N]> {
        let (taken, rest) = self.0.split_first_chunk::<N>()?;
        self.0 = rest;
        Some(*taken)
    }

    fn word(&mut self) -> Option<u64> {
        self.take().map(u64::from_le_bytes)
    }

    fn part(&mut self) -> Option<u32> {
        self.take().map(u32::from_le_bytes)
    }
}

impl Checkpoint {
    pub fn head(&self) -> OpId {
        OpId(self.head)
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(FIXED_LEN + self.workspaces.len() * WORKSPACE_LEN);
        out.push(VERSION);
        out.extend_from_slice(&self.boot);
        out.extend_from_slice(&self.generation.to_le_bytes());
        out.extend_from_slice(&self.seq.to_le_bytes());
        out.extend_from_slice(&self.head);
        for word in [self.head_at, self.head_end, self.synced] {
            out.extend_from_slice(&word.to_le_bytes());
        }
        for len in self.odb.lens {
            out.extend_from_slice(&len.to_le_bytes());
        }
        out.extend_from_slice(&self.odb.generation.to_le_bytes());
        for word in [self.linked_at, self.linked, self.migrated] {
            out.extend_from_slice(&word.to_le_bytes());
        }
        out.extend_from_slice(&(self.workspaces.len() as u32).to_le_bytes());
        for workspace in &self.workspaces {
            out.extend_from_slice(&workspace.id);
            for word in [workspace.last, workspace.undoable, workspace.index_at] {
                out.extend_from_slice(&word.to_le_bytes());
            }
            out.extend_from_slice(&workspace.index_sum);
            out.extend_from_slice(&workspace.step.to_le_bytes());
        }
        out
    }

    /// `None` for a body of another version or of another length than its
    /// count of workspaces gives.
    pub fn decode(body: &[u8]) -> Option<Self> {
        let mut read = Reader(body);
        if read.take::<1>()? != [VERSION] {
            return None;
        }
        let mut checkpoint = Checkpoint {
            boot: read.take()?,
            generation: read.part()?,
            seq: read.word()?,
            head: read.take()?,
            head_at: read.word()?,
            head_end: read.word()?,
            synced: read.word()?,
            ..Checkpoint::default()
        };
        for len in &mut checkpoint.odb.lens {
            *len = read.word()?;
        }
        checkpoint.odb.generation = read.part()?;
        checkpoint.linked_at = read.word()?;
        checkpoint.linked = read.word()?;
        checkpoint.migrated = read.word()?;
        let count = read.part()? as usize;
        if read.0.len() != count.checked_mul(WORKSPACE_LEN)? {
            return None;
        }
        for _ in 0..count {
            checkpoint.workspaces.push(Workspace {
                id: read.take()?,
                last: read.word()?,
                undoable: read.word()?,
                index_at: read.word()?,
                index_sum: read.take()?,
                step: read.word()?,
            });
        }
        Some(checkpoint)
    }
}
