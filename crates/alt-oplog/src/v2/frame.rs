//! Frames, the unit of a version 2 log: an op, a checkpoint or the receipt
//! of a request (all integers little-endian).
//!
//! ```text
//! header (48 bytes)
//! [0]      kind: 0 the block ends here, 1 op, 2 checkpoint, 3 receipt
//! [1]      flags: bit 0 its writer left it unsynced, bit 1 it can be
//!          undone, bit 2 nothing is appended after it by whoever does not
//!          know it, bit 7 a reader that does not know the kind skips it
//! [2..4)   length of the extension
//! [4..8)   length of the body
//! [8..16)  where the frame before it starts, zero for the first
//! [16..24) where the op before it of the same workspace starts, or zero
//! [24..32) up to where its writer knew the log synced
//! [32..40) the workspace it belongs to, zeros for none
//! [40..48) how many ops the log holds up to it, itself counted
//! extension: items of [tag u8][length u8][content]
//! body
//! trailer: an op's id (32 bytes) for an op, then for every kind
//!          [length of the whole frame u32][checksum u32]
//! ```
//!
//! The checksum is the CRC-32C of everything before it, begun from the
//! identity of the log and its generation: a frame written before records
//! were cut off the log does not read at the same place after.

use alt_hash::Crc32c;

use crate::v2::ext::word;
pub use crate::v2::ext::{Cut, Ext, OdbBounds, Step};
use crate::{OpId, OpLogError};

pub const HEADER_LEN: usize = 48;
/// the length and the checksum that close every frame
pub const CLOSE_LEN: usize = 8;

pub const KIND_OP: u8 = 1;
pub const KIND_CHECKPOINT: u8 = 2;
pub const KIND_RECEIPT: u8 = 3;

pub const UNSYNCED: u8 = 1;
pub const UNDOABLE: u8 = 1 << 1;
pub const REQUIRED: u8 = 1 << 2;
pub const SKIPPABLE: u8 = 1 << 7;

/// What the checksum of a frame begins from.
#[derive(Clone, Copy)]
pub struct Seed(Crc32c);

impl Seed {
    pub fn new(log: &[u8; 16], generation: u32) -> Self {
        let mut sum = Crc32c::new();
        sum.update(log);
        sum.update(&generation.to_le_bytes());
        Seed(sum)
    }

    pub fn sum(&self, bytes: &[u8]) -> u32 {
        let mut sum = self.0;
        sum.update(bytes);
        sum.finalize()
    }
}

/// The fixed fields of a frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Head {
    pub kind: u8,
    pub flags: u8,
    pub prev: u64,
    pub prev_in_workspace: u64,
    pub synced: u64,
    pub workspace: [u8; 8],
    pub seq: u64,
}

/// A frame as it reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct View<'a> {
    pub head: Head,
    pub ext: Ext,
    pub body: &'a [u8],
    /// the id an op frame closes with
    pub op: Option<OpId>,
}

/// How long the frame is that begins with `header`, by its kind and the
/// two lengths it gives; `None` for the mark that ends a block.
pub fn len_of(header: &[u8; HEADER_LEN]) -> Option<u64> {
    let ext = u16::from_le_bytes([header[2], header[3]]) as u64;
    let body = u32::from_le_bytes(header[4..8].try_into().unwrap()) as u64;
    let id = if header[0] == KIND_OP { 32 } else { 0 };
    (header[0] != 0).then_some(HEADER_LEN as u64 + ext + body + id + CLOSE_LEN as u64)
}

/// How long a frame is with an extension and a body of these lengths.
pub fn frame_len(ext: usize, body: usize, op: bool) -> u64 {
    (HEADER_LEN + ext + body + if op { 32 } else { 0 } + CLOSE_LEN) as u64
}

/// Where the body of the frame that begins with `header` starts in it.
pub fn body_at(header: &[u8; HEADER_LEN]) -> usize {
    HEADER_LEN + u16::from_le_bytes([header[2], header[3]]) as usize
}

/// The bytes of a frame, block headers not among them.
pub fn encode(
    seed: &Seed,
    head: &Head,
    ext: &Ext,
    body: &[u8],
    op: Option<&OpId>,
) -> Result<Vec<u8>, OpLogError> {
    let ext = ext.encode();
    let id = if op.is_some() { 32 } else { 0 };
    let len = HEADER_LEN + ext.len() + body.len() + id + CLOSE_LEN;
    if ext.len() > u16::MAX as usize || u32::try_from(len).is_err() {
        return Err(OpLogError::TooLarge(len));
    }
    let mut out = Vec::with_capacity(len);
    out.extend_from_slice(&[head.kind, head.flags]);
    out.extend_from_slice(&(ext.len() as u16).to_le_bytes());
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&head.prev.to_le_bytes());
    out.extend_from_slice(&head.prev_in_workspace.to_le_bytes());
    out.extend_from_slice(&head.synced.to_le_bytes());
    out.extend_from_slice(&head.workspace);
    out.extend_from_slice(&head.seq.to_le_bytes());
    out.extend_from_slice(&ext);
    out.extend_from_slice(body);
    if let Some(op) = op {
        out.extend_from_slice(&op.0);
    }
    out.extend_from_slice(&(len as u32).to_le_bytes());
    let sum = seed.sum(&out);
    out.extend_from_slice(&sum.to_le_bytes());
    Ok(out)
}

/// The frame `bytes` are, all of them and nothing else, when they read
/// under `seed`.
pub fn parse<'a>(seed: &Seed, bytes: &'a [u8]) -> Option<View<'a>> {
    let header: &[u8; HEADER_LEN] = bytes.first_chunk()?;
    if len_of(header)? != bytes.len() as u64 {
        return None;
    }
    let (covered, sum) = bytes.split_at(bytes.len() - 4);
    let said = u32::from_le_bytes(covered[covered.len() - 4..].try_into().unwrap());
    if said as usize != bytes.len() || seed.sum(covered) != u32::from_le_bytes(sum.try_into().ok()?)
    {
        return None;
    }
    let body_at = body_at(header);
    let body_len = u32::from_le_bytes(header[4..8].try_into().unwrap()) as usize;
    let body = &bytes[body_at..body_at + body_len];
    let op =
        (header[0] == KIND_OP).then(|| OpId(bytes[body_at + body_len..][..32].try_into().unwrap()));
    Some(View {
        head: Head {
            kind: header[0],
            flags: header[1],
            prev: word(header, 8),
            prev_in_workspace: word(header, 16),
            synced: word(header, 24),
            workspace: header[32..40].try_into().unwrap(),
            seq: word(header, 40),
        },
        ext: Ext::decode(&bytes[HEADER_LEN..body_at])?,
        body,
        op,
    })
}
