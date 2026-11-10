//! the input bit reader and the code tables of a block

use crate::{
    Error,
    huffman::{
        DISTANCE_BITS, DISTANCE_ENTRIES, Kind, LITLEN_BITS, LITLEN_ENTRIES, PRECODE_BITS,
        PRECODE_ENTRIES, build,
    },
};
use std::sync::OnceLock;

/// Input position and the bits taken from it but not used yet, lowest bit first.
pub(crate) struct Bits<'a> {
    pub(crate) input: &'a [u8],
    pub(crate) at: usize,
    pub(crate) hold: u64,
    pub(crate) count: u32,
}

impl Bits<'_> {
    /// takes whole bytes from the input while they fit
    #[inline]
    pub(crate) fn fill(&mut self) {
        while self.count <= 56 && self.at < self.input.len() {
            self.hold |= u64::from(self.input[self.at]) << self.count;
            self.at += 1;
            self.count += 8;
        }
    }

    /// the next `count` bits, or None when the input does not hold that many yet
    #[inline]
    pub(crate) fn take(&mut self, count: u32) -> Option<u32> {
        if self.count < count {
            self.fill();
            if self.count < count {
                return None;
            }
        }
        let value = (self.hold & ((1 << count) - 1)) as u32;
        self.hold >>= count;
        self.count -= count;
        Some(value)
    }

    /// hands whole unused bytes back to the input, optionally dropping the partial one
    pub(crate) fn give_back(&mut self, drop_partial_byte: bool) {
        self.at -= (self.count >> 3) as usize;
        self.count = if drop_partial_byte { 0 } else { self.count & 7 };
        self.hold &= (1 << self.count) - 1;
    }
}

pub(crate) struct Tables {
    pub(crate) litlen: [u32; LITLEN_ENTRIES],
    pub(crate) distance: [u32; DISTANCE_ENTRIES],
    /// the first-level index of each table as a bit mask
    pub(crate) litlen_mask: u64,
    pub(crate) distance_mask: u64,
}

impl Tables {
    pub(crate) fn empty() -> Box<Self> {
        Box::new(Self {
            litlen: [0; LITLEN_ENTRIES],
            distance: [0; DISTANCE_ENTRIES],
            litlen_mask: 0,
            distance_mask: 0,
        })
    }

    fn set(&mut self, literal_lengths: &[u8], distances: &[u8]) -> Result<(), Error> {
        let bits = build(
            &mut self.litlen,
            literal_lengths,
            Kind::LiteralLength,
            LITLEN_BITS,
        )?;
        self.litlen_mask = (1 << bits) - 1;
        let bits = build(&mut self.distance, distances, Kind::Distance, DISTANCE_BITS)?;
        self.distance_mask = (1 << bits) - 1;
        Ok(())
    }

    /// the tables of the code that blocks of the fixed kind share
    pub(crate) fn fixed() -> &'static Self {
        static FIXED: OnceLock<Box<Tables>> = OnceLock::new();
        FIXED.get_or_init(|| {
            let mut lengths = [8u8; 288];
            lengths[144..256].fill(9);
            lengths[256..280].fill(7);
            let mut tables = Tables::empty();
            tables
                .set(&lengths, &[5; 32])
                .expect("the fixed code is complete");
            tables
        })
    }
}

/// The description of a dynamic block's two codes, read piece by piece: the input may
/// run out between any two pieces.
pub(crate) struct Description {
    literal_lengths: usize,
    distances: usize,
    precodes: usize,
    /// how many lengths have been read in the current stage
    read: usize,
    lengths: [u8; 320],
    precode: [u32; PRECODE_ENTRIES],
    precode_mask: u64,
    stage: Stage,
}

#[derive(Clone, Copy, PartialEq)]
enum Stage {
    Counts,
    Precode,
    Lengths,
}

const PRECODE_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

impl Description {
    pub(crate) fn new() -> Self {
        Self {
            literal_lengths: 0,
            distances: 0,
            precodes: 0,
            read: 0,
            lengths: [0; 320],
            precode: [0; PRECODE_ENTRIES],
            precode_mask: 0,
            stage: Stage::Counts,
        }
    }

    pub(crate) fn restart(&mut self) {
        self.stage = Stage::Counts;
    }

    /// Reads on. Ok(true) once `tables` hold the block's codes, Ok(false) when the
    /// input ran out first.
    pub(crate) fn read(&mut self, bits: &mut Bits<'_>, tables: &mut Tables) -> Result<bool, Error> {
        if self.stage == Stage::Counts {
            let Some(counts) = bits.take(14) else {
                return Ok(false);
            };
            self.literal_lengths = 257 + (counts & 31) as usize;
            self.distances = 1 + (counts >> 5 & 31) as usize;
            self.precodes = 4 + (counts >> 10) as usize;
            if self.literal_lengths > 286 || self.distances > 30 {
                return Err(Error::Corrupt("too many length or distance codes"));
            }
            self.lengths[..19].fill(0);
            self.read = 0;
            self.stage = Stage::Precode;
        }
        if self.stage == Stage::Precode {
            while self.read < self.precodes {
                let Some(length) = bits.take(3) else {
                    return Ok(false);
                };
                self.lengths[PRECODE_ORDER[self.read]] = length as u8;
                self.read += 1;
            }
            let bits = build(
                &mut self.precode,
                &self.lengths[..19],
                Kind::Precode,
                PRECODE_BITS,
            )?;
            self.precode_mask = (1 << bits) - 1;
            self.read = 0;
            self.stage = Stage::Lengths;
        }
        let total = self.literal_lengths + self.distances;
        while self.read < total {
            bits.fill();
            let entry = self.precode[(bits.hold & self.precode_mask) as usize];
            let code_bits = entry & 0x3f;
            let symbol = entry >> 16;
            let (extra_bits, base) = match symbol {
                0..16 => (0, 1),
                16 => (2, 3),
                17 => (3, 3),
                _ => (7, 11),
            };
            if bits.count < code_bits + extra_bits {
                return Ok(false);
            }
            bits.hold >>= code_bits;
            let repeat = base + (bits.hold & ((1 << extra_bits) - 1)) as usize;
            bits.hold >>= extra_bits;
            bits.count -= code_bits + extra_bits;
            let value = match symbol {
                0..16 => symbol as u8,
                16 if self.read == 0 => {
                    return Err(Error::Corrupt("repeat of a length before the first"));
                }
                16 => self.lengths[self.read - 1],
                _ => 0,
            };
            if self.read + repeat > total {
                return Err(Error::Corrupt("more code lengths than announced"));
            }
            self.lengths[self.read..self.read + repeat].fill(value);
            self.read += repeat;
        }
        if self.lengths[256] == 0 {
            return Err(Error::Corrupt("no end-of-block code"));
        }
        let (literal_lengths, distances) = self.lengths[..total].split_at(self.literal_lengths);
        tables.set(literal_lengths, distances)?;
        self.stage = Stage::Counts;
        Ok(true)
    }
}
