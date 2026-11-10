//! the decoder state that carries a stream across calls

use crate::{
    Error, Format,
    blocks::{Bits, Description, Tables},
    fast,
    huffman::{LITERAL, POINTER, SPECIAL},
    wrapper,
};

const WINDOW: usize = 32768;

/// what one call did
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Step {
    /// input bytes used; the rest must be given again, with more after it
    pub consumed: usize,
    /// output bytes written
    pub produced: usize,
    /// the stream is complete, checksum included
    pub done: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Header,
    Block,
    StoredLength,
    Stored,
    Description,
    Codes,
    /// a match that did not fit in the output of the previous call
    Match,
    Trailer,
    Done,
}

pub struct Inflater {
    format: Format,
    phase: Phase,
    hold: u64,
    hold_count: u32,
    last_block: bool,
    fixed_block: bool,
    stored_left: usize,
    match_left: usize,
    match_distance: usize,
    description: Description,
    tables: Box<Tables>,
    /// the last output of earlier calls, as a ring that `window_at` points into
    window: Vec<u8>,
    window_at: usize,
    adler: u32,
    crc: alt_hash::Crc32,
    total_in: u64,
    total_out: u64,
}

impl Inflater {
    pub fn new(format: Format) -> Self {
        Self {
            format,
            phase: Phase::Header,
            hold: 0,
            hold_count: 0,
            last_block: false,
            fixed_block: false,
            stored_left: 0,
            match_left: 0,
            match_distance: 0,
            description: Description::new(),
            tables: Tables::empty(),
            window: Vec::new(),
            window_at: 0,
            adler: 1,
            crc: alt_hash::Crc32::new(),
            total_in: 0,
            total_out: 0,
        }
    }

    /// ready for another stream of the same format
    pub fn reset(&mut self) {
        self.restart(self.format);
    }

    /// ready for a stream of `format`
    pub(crate) fn restart(&mut self, format: Format) {
        self.format = format;
        self.phase = Phase::Header;
        self.hold = 0;
        self.hold_count = 0;
        self.window.clear();
        self.window_at = 0;
        self.adler = 1;
        self.crc = alt_hash::Crc32::new();
        self.total_in = 0;
        self.total_out = 0;
    }

    /// input bytes used since the stream began
    pub fn total_in(&self) -> u64 {
        self.total_in
    }

    /// output bytes written since the stream began
    pub fn total_out(&self) -> u64 {
        self.total_out
    }

    /// Decodes from `input` into `output` until the stream ends, the input runs out or
    /// the output is full. Input that was not consumed must be passed again.
    pub fn inflate(&mut self, input: &[u8], output: &mut [u8]) -> Result<Step, Error> {
        let mut bits = Bits {
            input,
            at: 0,
            hold: self.hold,
            count: self.hold_count,
        };
        let mut written = 0;
        let outcome = self.run(&mut bits, output, &mut written);
        // what follows the last block starts at a whole byte
        bits.give_back(self.phase == Phase::Trailer);
        self.hold = bits.hold;
        self.hold_count = bits.count;
        outcome?;
        let produced = &output[..written];
        match self.format {
            Format::Zlib => self.adler = alt_hash::adler32(self.adler, produced),
            Format::Gzip => self.crc.update(produced),
            Format::Raw => {}
        }
        self.total_out += written as u64;
        if self.phase == Phase::Trailer && self.trailer(&mut bits)? {
            self.phase = Phase::Done;
        }
        if self.phase != Phase::Done {
            self.remember(produced);
        }
        self.total_in += bits.at as u64;
        Ok(Step {
            consumed: bits.at,
            produced: written,
            done: self.phase == Phase::Done,
        })
    }

    /// keeps the end of this call's output for matches that later calls reach back to
    fn remember(&mut self, produced: &[u8]) {
        if self.window.len() < WINDOW && self.window.len() + produced.len() <= WINDOW {
            self.window.extend_from_slice(produced);
            self.window_at = self.window.len() % WINDOW;
            return;
        }
        self.window.resize(WINDOW, 0);
        let tail = &produced[produced.len().saturating_sub(WINDOW)..];
        let first = tail.len().min(WINDOW - self.window_at);
        self.window[self.window_at..self.window_at + first].copy_from_slice(&tail[..first]);
        self.window[..tail.len() - first].copy_from_slice(&tail[first..]);
        self.window_at = (self.window_at + tail.len()) % WINDOW;
    }

    /// Checks what follows the last block. False when the input does not hold it yet.
    fn trailer(&mut self, bits: &mut Bits<'_>) -> Result<bool, Error> {
        let rest = &bits.input[bits.at..];
        match self.format {
            Format::Raw => {}
            Format::Zlib => {
                let Some(stored) = rest.first_chunk::<4>() else {
                    return Ok(false);
                };
                if u32::from_be_bytes(*stored) != self.adler {
                    return Err(Error::Checksum);
                }
                bits.at += 4;
            }
            Format::Gzip => {
                let Some(stored) = rest.first_chunk::<8>() else {
                    return Ok(false);
                };
                let (crc, length) = stored.split_at(4);
                if u32::from_le_bytes(crc.try_into().unwrap()) != self.crc.finalize()
                    || u32::from_le_bytes(length.try_into().unwrap()) != self.total_out as u32
                {
                    return Err(Error::Checksum);
                }
                bits.at += 8;
            }
        }
        Ok(true)
    }

    fn run(
        &mut self,
        bits: &mut Bits<'_>,
        output: &mut [u8],
        written: &mut usize,
    ) -> Result<(), Error> {
        loop {
            match self.phase {
                Phase::Header => {
                    let length = match self.format {
                        Format::Raw => Some(0),
                        Format::Zlib => wrapper::zlib_header(bits.input)?,
                        Format::Gzip => wrapper::gzip_header(bits.input)?,
                    };
                    let Some(length) = length else {
                        return Ok(());
                    };
                    bits.at = length;
                    self.phase = Phase::Block;
                }
                Phase::Block => {
                    let Some(header) = bits.take(3) else {
                        return Ok(());
                    };
                    self.last_block = header & 1 != 0;
                    self.phase = match header >> 1 {
                        0 => Phase::StoredLength,
                        1 => {
                            self.fixed_block = true;
                            Phase::Codes
                        }
                        2 => {
                            self.fixed_block = false;
                            self.description.restart();
                            Phase::Description
                        }
                        _ => return Err(Error::Corrupt("unknown block type")),
                    };
                }
                Phase::StoredLength => {
                    // the length starts at the next whole byte
                    bits.give_back(true);
                    let Some(lengths) = bits.input[bits.at..].first_chunk::<4>() else {
                        return Ok(());
                    };
                    let length = u16::from_le_bytes([lengths[0], lengths[1]]);
                    if length != !u16::from_le_bytes([lengths[2], lengths[3]]) {
                        return Err(Error::Corrupt("stored block lengths disagree"));
                    }
                    bits.at += 4;
                    self.stored_left = usize::from(length);
                    self.phase = Phase::Stored;
                }
                Phase::Stored => {
                    let take = self
                        .stored_left
                        .min(bits.input.len() - bits.at)
                        .min(output.len() - *written);
                    output[*written..*written + take]
                        .copy_from_slice(&bits.input[bits.at..bits.at + take]);
                    bits.at += take;
                    *written += take;
                    self.stored_left -= take;
                    if self.stored_left != 0 {
                        return Ok(());
                    }
                    self.end_of_block();
                }
                Phase::Description => {
                    if !self.description.read(bits, &mut self.tables)? {
                        return Ok(());
                    }
                    self.phase = Phase::Codes;
                }
                Phase::Codes => {
                    let tables = if self.fixed_block {
                        Tables::fixed()
                    } else {
                        &*self.tables
                    };
                    if fast::worthwhile(bits, output, *written) {
                        let earlier = self.window.len();
                        if fast::run(tables, bits, output, written, earlier)? {
                            self.end_of_block();
                            continue;
                        }
                    }
                    match symbol(tables, bits, output, written, self.window.len())? {
                        Symbol::Literal => {}
                        Symbol::EndOfBlock => self.end_of_block(),
                        Symbol::Match { length, distance } => {
                            self.match_left = length;
                            self.match_distance = distance;
                            self.phase = Phase::Match;
                        }
                        Symbol::Pause => return Ok(()),
                    }
                }
                Phase::Match => {
                    if !self.copy(output, written) {
                        return Ok(());
                    }
                    self.phase = Phase::Codes;
                }
                Phase::Trailer | Phase::Done => return Ok(()),
            }
        }
    }

    fn end_of_block(&mut self) {
        self.phase = if self.last_block {
            Phase::Trailer
        } else {
            Phase::Block
        };
    }

    /// copies as much of the pending match as the output takes; true once all of it
    fn copy(&mut self, output: &mut [u8], written: &mut usize) -> bool {
        let take = self.match_left.min(output.len() - *written);
        for _ in 0..take {
            output[*written] = if self.match_distance <= *written {
                output[*written - self.match_distance]
            } else {
                // from the output of earlier calls
                let back = self.match_distance - *written;
                self.window[(self.window_at + self.window.len() - back) % self.window.len()]
            };
            *written += 1;
        }
        self.match_left -= take;
        self.match_left == 0
    }
}

enum Symbol {
    Literal,
    EndOfBlock,
    Match {
        length: usize,
        distance: usize,
    },
    /// the input or the output cannot take this symbol yet
    Pause,
}

/// Decodes one literal, match or end of block with every check: used near the ends
/// of the input and output, where the fast loop may not run. `earlier` is how much
/// output of earlier calls a match may reach back into.
fn symbol(
    tables: &Tables,
    bits: &mut Bits<'_>,
    output: &mut [u8],
    written: &mut usize,
    earlier: usize,
) -> Result<Symbol, Error> {
    bits.fill();
    // nothing is taken from `bits` until the whole symbol is known to be there
    let mut hold = bits.hold;
    let mut used = 0;
    let mut entry = tables.litlen[(hold & tables.litlen_mask) as usize];
    if entry & POINTER != 0 {
        hold >>= entry & 0x3f;
        used += entry & 0x3f;
        let index = hold & ((1 << (entry >> 8 & 0x1f)) - 1);
        entry = tables.litlen[(entry >> 16) as usize + index as usize];
    }
    hold >>= entry & 0x3f;
    used += entry & 0x3f;
    if used > bits.count {
        return Ok(Symbol::Pause);
    }
    let decoded = if entry & LITERAL != 0 {
        let Some(slot) = output.get_mut(*written) else {
            return Ok(Symbol::Pause);
        };
        *slot = (entry >> 16) as u8;
        *written += 1;
        Symbol::Literal
    } else if entry & SPECIAL != 0 {
        if entry >> 16 != 0 {
            return Err(Error::Corrupt("undefined literal or length code"));
        }
        Symbol::EndOfBlock
    } else {
        let extra = entry >> 8 & 0x1f;
        let length = (entry >> 16) as usize + (hold & ((1 << extra) - 1)) as usize;
        hold >>= extra;
        used += extra;
        let mut entry = tables.distance[(hold & tables.distance_mask) as usize];
        if entry & POINTER != 0 {
            hold >>= entry & 0x3f;
            used += entry & 0x3f;
            let index = hold & ((1 << (entry >> 8 & 0x1f)) - 1);
            entry = tables.distance[(entry >> 16) as usize + index as usize];
        }
        hold >>= entry & 0x3f;
        used += entry & 0x3f;
        let extra = entry >> 8 & 0x1f;
        used += extra;
        if used > bits.count {
            return Ok(Symbol::Pause);
        }
        if entry & SPECIAL != 0 {
            return Err(Error::Corrupt("undefined distance code"));
        }
        let distance = (entry >> 16) as usize + (hold & ((1 << extra) - 1)) as usize;
        if distance > *written + earlier {
            return Err(Error::Corrupt("match reaches before the start of the data"));
        }
        Symbol::Match { length, distance }
    };
    bits.hold >>= used;
    bits.count -= used;
    Ok(decoded)
}
