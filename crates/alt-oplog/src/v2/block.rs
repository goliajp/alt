//! The file header and the blocks of a version 2 log.
//!
//! ```text
//! file header (64 bytes)
//! [0..4)   "ALTL"            [4]      version, 2
//! [5]      the block size as a power of two
//! [8..12)  features a reader must know to read or write the log; bit 0
//!          for a log of the format before it is settled
//! [12..16) features a writer must know to write it
//! [16..20) features nobody has to know
//! [20..36) the identity of this log, random
//! [36..44) when it was created, in milliseconds
//! [56..64) checksum = blake3(the 56 bytes before it)[..8]
//!
//! block header (32 bytes), at every multiple of the block size but zero
//! [0..4)   "ALTB"            [4..8)   the number of the block
//! [8..12)  where in the block the first frame starts; zero when a frame
//!          begun earlier fills all of it
//! [12..20) the checkpoint frame last written before this header, or zero
//! [20..24) the generation of the log
//! [24..28) checksum = crc32c(the identity of the log, the 24 bytes)
//! [28..32) zeros
//! ```
//!
//! A frame that fits into what is left of a block lies in it whole. One
//! that does not begins a new block: the rest of the old one is zeros, the
//! new one opens with its header and a checkpoint. Only a frame that does
//! not fit after that checkpoint either runs on across block headers.

use std::borrow::Cow;

use alt_hash::Crc32c;

use crate::OpLogError;

pub const FILE_HEADER_LEN: usize = 64;
pub const BLOCK_HEADER_LEN: usize = 32;
pub const VERSION: u8 = 2;
/// Blocks of 16 KiB: an open reads up to a block, and takes some 90
/// thousand instructions for a full one of this size.
pub const BLOCK_EXP: u8 = 14;
/// Among the features a build must know to read a log: the log is laid
/// out as builds lay it while the format is not settled. The build that
/// settles the format writes no such log and opens none.
pub const DRAFT: u32 = 1;

const MAGIC: [u8; 4] = *b"ALTL";
const BLOCK_MAGIC: [u8; 4] = *b"ALTB";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileHeader {
    pub block_exp: u8,
    pub incompatible: u32,
    pub read_compatible: u32,
    pub compatible: u32,
    pub id: [u8; 16],
    pub created_ms: u64,
}

impl FileHeader {
    pub fn encode(&self) -> [u8; FILE_HEADER_LEN] {
        let mut out = [0u8; FILE_HEADER_LEN];
        out[..4].copy_from_slice(&MAGIC);
        out[4] = VERSION;
        out[5] = self.block_exp;
        out[8..12].copy_from_slice(&self.incompatible.to_le_bytes());
        out[12..16].copy_from_slice(&self.read_compatible.to_le_bytes());
        out[16..20].copy_from_slice(&self.compatible.to_le_bytes());
        out[20..36].copy_from_slice(&self.id);
        out[36..44].copy_from_slice(&self.created_ms.to_le_bytes());
        let sum = alt_hash::blake3::hash(&out[..56]);
        out[56..].copy_from_slice(&sum.as_bytes()[..8]);
        out
    }

    /// The header `bytes` begin with. A log with a feature this build
    /// must know to read it is refused.
    pub fn decode(bytes: &[u8]) -> Result<Self, OpLogError> {
        let Some(bytes) = bytes.first_chunk::<FILE_HEADER_LEN>() else {
            return Err(OpLogError::Format("oplog header cut short"));
        };
        if bytes[..4] != MAGIC {
            return Err(OpLogError::Format("bad oplog header"));
        }
        if bytes[4] != VERSION {
            return Err(OpLogError::Format("unsupported oplog version"));
        }
        let sum = alt_hash::blake3::hash(&bytes[..56]);
        if sum.as_bytes()[..8] != bytes[56..] || !(10..=30).contains(&bytes[5]) {
            return Err(OpLogError::Format("oplog header corrupt"));
        }
        let part = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
        let header = FileHeader {
            block_exp: bytes[5],
            incompatible: part(8),
            read_compatible: part(12),
            compatible: part(16),
            id: bytes[20..36].try_into().unwrap(),
            created_ms: u64::from_le_bytes(bytes[36..44].try_into().unwrap()),
        };
        if header.incompatible & !DRAFT != 0 {
            return Err(OpLogError::Format(
                "the oplog uses a feature this alt does not know",
            ));
        }
        if header.incompatible & DRAFT == 0 {
            return Err(OpLogError::Format(
                "the oplog is not of the draft format this alt reads",
            ));
        }
        Ok(header)
    }

    /// Whether this build may append to the log.
    pub fn writable(&self) -> bool {
        self.read_compatible == 0
    }

    pub fn layout(&self) -> Layout {
        Layout {
            block: 1 << self.block_exp,
            id: self.id,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockHeader {
    pub index: u32,
    pub first: u32,
    pub checkpoint: u64,
    pub generation: u32,
}

/// How a log is laid out in blocks, and whose blocks they are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub block: u64,
    pub id: [u8; 16],
}

impl Layout {
    /// How many bytes lie between `at` and the end of its block.
    pub fn room(&self, at: u64) -> u64 {
        self.block - at % self.block
    }

    /// Whether a block header lies at `at`.
    pub fn opens_block(&self, at: u64) -> bool {
        at != 0 && at.is_multiple_of(self.block)
    }

    fn header_sum(&self, bytes: &[u8]) -> u32 {
        let mut sum = Crc32c::new();
        sum.update(&self.id);
        sum.update(bytes);
        sum.finalize()
    }

    pub fn encode_header(&self, header: &BlockHeader) -> [u8; BLOCK_HEADER_LEN] {
        let mut out = [0u8; BLOCK_HEADER_LEN];
        out[..4].copy_from_slice(&BLOCK_MAGIC);
        out[4..8].copy_from_slice(&header.index.to_le_bytes());
        out[8..12].copy_from_slice(&header.first.to_le_bytes());
        out[12..20].copy_from_slice(&header.checkpoint.to_le_bytes());
        out[20..24].copy_from_slice(&header.generation.to_le_bytes());
        let sum = self.header_sum(&out[..24]);
        out[24..28].copy_from_slice(&sum.to_le_bytes());
        out
    }

    /// The header of the block that starts at `at`, where `bytes` begin
    /// with one that reads and is of that block.
    pub fn decode_header(&self, bytes: &[u8], at: u64) -> Option<BlockHeader> {
        let bytes = bytes.first_chunk::<BLOCK_HEADER_LEN>()?;
        let part = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
        let header = BlockHeader {
            index: part(4),
            first: part(8),
            checkpoint: u64::from_le_bytes(bytes[12..20].try_into().unwrap()),
            generation: part(20),
        };
        let holds = bytes[..4] == BLOCK_MAGIC
            && part(24) == self.header_sum(&bytes[..24])
            && part(28) == 0
            && header.index as u64 * self.block == at;
        holds.then_some(header)
    }

    /// Where a frame of `len` bytes that starts at `at` ends in the file,
    /// the block headers it runs across counted.
    pub fn end_of(&self, at: u64, len: u64) -> u64 {
        let room = self.room(at);
        if len <= room {
            return at + len;
        }
        let each = self.block - BLOCK_HEADER_LEN as u64;
        let rest = len - room;
        at + len + rest.div_ceil(each) * BLOCK_HEADER_LEN as u64
    }

    /// Appends to `out` the bytes of `frame` as they lie in the file from
    /// `at` on: a header where it runs into another block, which says
    /// where in that block the frame ends.
    pub fn lay(&self, at: u64, frame: &[u8], checkpoint: u64, generation: u32, out: &mut Vec<u8>) {
        let room = (self.room(at) as usize).min(frame.len());
        out.extend_from_slice(&frame[..room]);
        let each = (self.block as usize) - BLOCK_HEADER_LEN;
        let next = (at / self.block) as u32 + 1;
        for (index, part) in (next..).zip(frame[room..].chunks(each)) {
            let first = match part.len() == each {
                true => 0,
                false => (BLOCK_HEADER_LEN + part.len()) as u32,
            };
            out.extend_from_slice(&self.encode_header(&BlockHeader {
                index,
                first,
                checkpoint,
                generation,
            }));
            out.extend_from_slice(part);
        }
    }

    /// The `len` bytes of a frame that starts at `at`, read out of `data`
    /// (which begins at byte `base` of the file) with the block headers it
    /// runs across left out, and where it ends in the file. `None` where
    /// `data` does not hold all of it or one of those headers does not
    /// read as a header inside this frame.
    pub fn lift<'a>(
        &self,
        data: &'a [u8],
        base: u64,
        at: u64,
        len: u64,
    ) -> Option<(Cow<'a, [u8]>, u64)> {
        let end = self.end_of(at, len);
        let held = data.get(at.checked_sub(base)? as usize..(end - base) as usize)?;
        let room = self.room(at);
        if len <= room {
            return Some((Cow::Borrowed(held), end));
        }
        let (first, mut rest) = held.split_at(room as usize);
        let mut frame = Vec::with_capacity(len as usize);
        frame.extend_from_slice(first);
        let mut block = at + room;
        while !rest.is_empty() {
            let header = self.decode_header(rest, block)?;
            let left = len as usize - frame.len();
            let each = self.block as usize - BLOCK_HEADER_LEN;
            let expected = if left >= each {
                0
            } else {
                BLOCK_HEADER_LEN + left
            };
            if header.first as usize != expected {
                return None;
            }
            let part = left.min(each);
            frame.extend_from_slice(&rest[BLOCK_HEADER_LEN..BLOCK_HEADER_LEN + part]);
            rest = &rest[BLOCK_HEADER_LEN + part..];
            block += self.block;
        }
        Some((Cow::Owned(frame), end))
    }
}
