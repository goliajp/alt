//! Huffman decoding of literals: tree description, single stream and four
//! interleaved streams.

use crate::Error;
use crate::bits::{BackReader, load64};
use crate::fse::{Entry, build_table, read_distribution};

/// Longest code the format allows.
pub(crate) const MAX_BITS: u32 = 11;

/// Symbols decoded per stream between two refills; `5 * MAX_BITS` bits fit
/// in what a refill guarantees.
const BURST: usize = 5;

/// Lookup table indexed by the next `log` bits of a stream. An entry holds
/// the symbol in its low byte and the code length above it.
pub(crate) struct HufTable {
    entries: [u16; 1 << MAX_BITS],
    log: u32,
}

impl HufTable {
    pub(crate) fn new() -> Self {
        Self {
            entries: [0; 1 << MAX_BITS],
            log: 0,
        }
    }

    /// Parses a tree description and returns the bytes it occupied.
    pub(crate) fn read(&mut self, src: &[u8]) -> Result<usize, Error> {
        let mut weights = [0u8; 256];
        let (count, used) = read_weights(src, &mut weights)?;
        let mut rank = [0u32; MAX_BITS as usize + 2];
        let mut total = 0u32;
        for &weight in &weights[..count] {
            if weight as u32 > MAX_BITS {
                return Err(Error::Corrupt);
            }
            rank[weight as usize] += 1;
            total += (1 << weight) >> 1;
        }
        if total == 0 {
            return Err(Error::Corrupt);
        }
        let log = total.ilog2() + 1;
        if log > MAX_BITS {
            return Err(Error::Corrupt);
        }
        // the last weight is implied: it completes the code space
        let rest = (1u32 << log) - total;
        if !rest.is_power_of_two() {
            return Err(Error::Corrupt);
        }
        let last = rest.ilog2() + 1;
        weights[count] = last as u8;
        rank[last as usize] += 1;
        if rank[1] < 2 || rank[1] & 1 != 0 {
            return Err(Error::Corrupt);
        }

        let mut start = [0usize; MAX_BITS as usize + 2];
        let mut at = 0;
        for weight in 1..=log as usize {
            start[weight] = at;
            at += (rank[weight] as usize) << (weight - 1);
        }
        for (symbol, &weight) in weights[..=count].iter().enumerate() {
            if weight == 0 {
                continue;
            }
            let span = 1usize << (weight - 1);
            let entry = ((log + 1 - weight as u32) << 8) as u16 | symbol as u16;
            let from = start[weight as usize];
            self.entries[from..from + span].fill(entry);
            start[weight as usize] += span;
        }
        self.log = log;
        Ok(used)
    }

    /// Decodes one stream into `count` bytes at `dst`.
    ///
    /// # Safety
    /// `dst` must be valid for `count` bytes of writes.
    pub(crate) unsafe fn decode_stream(
        &self,
        src: &[u8],
        dst: *mut u8,
        count: usize,
    ) -> Result<(), Error> {
        let reader = BackReader::new(src)?;
        // safety: forwarded from the caller
        unsafe { self.drain(reader, dst, count) }
    }

    /// Decodes `count` symbols from `reader`, which must then be exhausted.
    ///
    /// # Safety
    /// `dst` must be valid for `count` bytes of writes.
    unsafe fn drain(
        &self,
        mut reader: BackReader<'_>,
        mut dst: *mut u8,
        count: usize,
    ) -> Result<(), Error> {
        let mut left = count;
        while left != 0 {
            reader.refill();
            let burst = left.min(BURST);
            for _ in 0..burst {
                let index = reader.peek(self.log) as usize;
                // safety: peek(log) < 1 << log <= entries.len(), and the
                // caller provides room for every symbol written
                unsafe {
                    let entry = *self.entries.get_unchecked(index);
                    reader.skip((entry >> 8) as u32);
                    dst.write(entry as u8);
                    dst = dst.add(1);
                }
            }
            left -= burst;
        }
        reader.refill();
        if reader.finished() {
            Ok(())
        } else {
            Err(Error::Corrupt)
        }
    }

    /// Decodes four streams, laid out behind a 6-byte jump table, into
    /// `count` bytes at `dst`.
    ///
    /// # Safety
    /// `dst` must be valid for `count` bytes of writes.
    pub(crate) unsafe fn decode_4x(
        &self,
        src: &[u8],
        dst: *mut u8,
        count: usize,
    ) -> Result<(), Error> {
        if src.len() < 10 || count < 6 {
            return Err(Error::Corrupt);
        }
        let size = |at: usize| u16::from_le_bytes([src[at], src[at + 1]]) as usize;
        let (first, rest) = src[6..].split_at_checked(size(0)).ok_or(Error::Corrupt)?;
        let (second, rest) = rest.split_at_checked(size(2)).ok_or(Error::Corrupt)?;
        let (third, fourth) = rest.split_at_checked(size(4)).ok_or(Error::Corrupt)?;
        let streams = [first, second, third, fourth];
        let segment = count.div_ceil(4);
        let lens = [segment, segment, segment, count - 3 * segment];

        let mut done = [0usize; 4];
        let mut readers = [None, None, None, None];
        if streams.iter().all(|s| s.len() >= 8 && s[s.len() - 1] != 0) {
            // safety: forwarded from the caller; the four output segments are
            // disjoint parts of dst..dst + count
            unsafe { self.burst_4x(streams, dst, lens, &mut done, &mut readers) };
        }
        let mut out = dst;
        for i in 0..4 {
            let reader = match readers[i].take() {
                Some(reader) => reader,
                None => BackReader::new(streams[i])?,
            };
            // safety: out + done[i] .. out + lens[i] lies inside the segment
            unsafe {
                self.drain(reader, out.add(done[i]), lens[i] - done[i])?;
                out = out.add(lens[i]);
            }
        }
        Ok(())
    }

    /// Decodes the four streams in lockstep while each has both input and
    /// output to spare, then hands back where each one stopped.
    ///
    /// # Safety
    /// Every stream must be at least 8 bytes long with a nonzero last byte,
    /// and `dst` must be valid for the sum of `lens` bytes of writes.
    unsafe fn burst_4x<'a>(
        &self,
        streams: [&'a [u8]; 4],
        dst: *mut u8,
        lens: [usize; 4],
        done: &mut [usize; 4],
        readers: &mut [Option<BackReader<'a>>; 4],
    ) {
        let table = self.entries.as_ptr();
        let shift = 64 - self.log;
        // safety (whole block): `ip[i]` stays inside its stream because a
        // round runs only while ip[i] >= start + 8 and moves it down by at
        // most 7 bytes; `op[i]` advances BURST bytes only when that many
        // remain in its segment; table indices are below 1 << log
        unsafe {
            let mut ip = streams.map(|s| s.as_ptr().add(s.len() - 8));
            let floor = streams.map(|s| s.as_ptr().add(8));
            // bit 0 is a sentinel: its position counts the bits consumed
            let mut bits = [0u64; 4];
            for i in 0..4 {
                let pad = streams[i][streams[i].len() - 1].leading_zeros() + 1;
                bits[i] = (load64(ip[i]) | 1) << pad;
            }
            let mut op = [
                dst,
                dst.add(lens[0]),
                dst.add(lens[0] + lens[1]),
                dst.add(lens[0] + lens[1] + lens[2]),
            ];
            let mut left = lens;
            while (0..4).all(|i| ip[i] >= floor[i] && left[i] >= BURST) {
                for _ in 0..BURST {
                    for i in 0..4 {
                        let entry = *table.add((bits[i] >> shift) as usize);
                        op[i].write(entry as u8);
                        op[i] = op[i].add(1);
                        bits[i] <<= entry >> 8;
                    }
                }
                for i in 0..4 {
                    let used = bits[i].trailing_zeros();
                    ip[i] = ip[i].sub((used >> 3) as usize);
                    bits[i] = (load64(ip[i]) | 1) << (used & 7);
                    left[i] -= BURST;
                }
            }
            for i in 0..4 {
                done[i] = lens[i] - left[i];
                let pos = ip[i].offset_from(streams[i].as_ptr()) as usize;
                readers[i] = Some(BackReader::resume(
                    streams[i],
                    pos,
                    bits[i].trailing_zeros(),
                ));
            }
        }
    }
}

/// Reads the weights of all symbols but the last. Returns how many there are
/// and the bytes consumed.
fn read_weights(src: &[u8], weights: &mut [u8; 256]) -> Result<(usize, usize), Error> {
    let header = *src.first().ok_or(Error::Corrupt)? as usize;
    if header >= 128 {
        // four bits per weight
        let count = header - 127;
        let bytes = count.div_ceil(2);
        let body = src.get(1..1 + bytes).ok_or(Error::Corrupt)?;
        for (i, weight) in weights[..count].iter_mut().enumerate() {
            let pair = body[i / 2];
            *weight = if i & 1 == 0 { pair >> 4 } else { pair & 15 };
        }
        Ok((count, 1 + bytes))
    } else {
        let body = src.get(1..1 + header).ok_or(Error::Corrupt)?;
        Ok((fse_weights(body, weights)?, 1 + header))
    }
}

/// Decodes weights coded with two interleaved FSE states. The stream holds
/// no count: it ends when a state update runs out of bits.
fn fse_weights(src: &[u8], weights: &mut [u8; 256]) -> Result<usize, Error> {
    const MAX_LOG: u32 = 6;
    let mut norm = [0i16; 256];
    let (log, symbols, used) = read_distribution(src, &mut norm, MAX_LOG)?;
    let mut table = [Entry::default(); 1 << MAX_LOG];
    build_table(&norm[..symbols], log, &mut table, 0, |symbol| {
        (symbol as u32, 0)
    });

    let mut reader = BackReader::new(&src[used..])?;
    let mut states = [reader.read(log) as usize, reader.read(log) as usize];
    reader.refill();
    if reader.overflowed() {
        return Err(Error::Corrupt);
    }
    let mut count = 0;
    let mut turn = 0;
    loop {
        // the last weight is never stored, so 255 is the most there can be
        if count > 253 {
            return Err(Error::Corrupt);
        }
        let entry = table[states[turn]];
        weights[count] = entry.base() as u8;
        count += 1;
        states[turn] = entry.next() + reader.read(entry.bits()) as usize;
        reader.refill();
        turn ^= 1;
        if reader.overflowed() {
            weights[count] = table[states[turn]].base() as u8;
            return Ok(count + 1);
        }
    }
}
