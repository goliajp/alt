//! Huffman decoding of literals: tree description, single stream and four
//! interleaved streams.

use crate::Error;
use crate::bits::{BackReader, load64};
use crate::weights::read_weights;

/// Longest code the format allows.
pub(crate) const MAX_BITS: u32 = 11;

/// Symbols decoded per stream between two refills; `5 * MAX_BITS` bits fit
/// in what a refill guarantees.
const BURST: usize = 5;

/// The two-symbol table is used when sixteen times the compressed size is
/// at most this many times the literal count: beyond that, codes are too
/// long for pairs to be common.
const PAIRS_RATIO: usize = 13;

/// Literal count from which the two-symbol table pays for its construction.
const PAIRS_MIN: usize = 8 << 10;

/// Lookup tables indexed by the next `log` bits of a stream.
pub(crate) struct HufTable {
    /// the symbol in the low byte, its code length above it
    entries: [u16; 1 << MAX_BITS],
    /// up to two symbols at once: the symbols in the low two bytes (first
    /// one lowest), then the bits they take, then how many they are
    pairs: [u32; 1 << MAX_BITS],
    /// whether `pairs` matches `entries`
    pairs_ready: bool,
    log: u32,
}

impl HufTable {
    pub(crate) fn new() -> Self {
        Self {
            entries: [0; 1 << MAX_BITS],
            pairs: [0; 1 << MAX_BITS],
            pairs_ready: false,
            log: 0,
        }
    }

    /// Derives the two-symbol table: wherever the bits left after one code
    /// hold all of the next code, the entry yields both.
    fn build_pairs(&mut self) {
        let size = 1usize << self.log;
        for index in 0..size {
            let first = self.entries[index] as u32;
            let first_len = first >> 8;
            let second = self.entries[(index << first_len) & (size - 1)] as u32;
            let both = first_len + (second >> 8);
            self.pairs[index] = if both <= self.log {
                (first & 0xFF) | (second & 0xFF) << 8 | both << 16 | 2 << 24
            } else {
                (first & 0xFF) | first_len << 16 | 1 << 24
            };
        }
        self.pairs_ready = true;
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
        self.pairs_ready = false;
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
        &mut self,
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
            // two symbols per lookup win when codes are short and there
            // are enough literals to repay building the table
            let pairs = count >= PAIRS_MIN && src.len() * 16 <= count * PAIRS_RATIO;
            if pairs && !self.pairs_ready {
                self.build_pairs();
            }
            // safety: forwarded from the caller; the four output segments are
            // disjoint parts of dst..dst + count
            unsafe { self.burst_4x(streams, dst, lens, pairs, &mut done, &mut readers) };
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
        pairs: bool,
        done: &mut [usize; 4],
        readers: &mut [Option<BackReader<'a>>; 4],
    ) {
        #[cfg(target_arch = "x86_64")]
        if std::is_x86_feature_detected!("bmi2") {
            // safety: the processor was just found to have the feature; the
            // rest is forwarded from the caller
            return unsafe { self.burst_4x_bmi2(streams, dst, lens, pairs, done, readers) };
        }
        // safety: forwarded from the caller
        unsafe { self.burst_4x_any(streams, dst, lens, pairs, done, readers) }
    }

    /// The same loop compiled for processors with BMI2, where the variable
    /// shifts of bit reading need no flags and no fixed register.
    ///
    /// # Safety
    /// As for [`HufTable::burst_4x`].
    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "bmi2")]
    unsafe fn burst_4x_bmi2<'a>(
        &self,
        streams: [&'a [u8]; 4],
        dst: *mut u8,
        lens: [usize; 4],
        pairs: bool,
        done: &mut [usize; 4],
        readers: &mut [Option<BackReader<'a>>; 4],
    ) {
        // safety: forwarded from the caller
        unsafe { self.burst_4x_any(streams, dst, lens, pairs, done, readers) }
    }

    /// # Safety
    /// As for [`HufTable::burst_4x`].
    #[inline(always)]
    unsafe fn burst_4x_any<'a>(
        &self,
        streams: [&'a [u8]; 4],
        dst: *mut u8,
        lens: [usize; 4],
        pairs: bool,
        done: &mut [usize; 4],
        readers: &mut [Option<BackReader<'a>>; 4],
    ) {
        let table = self.entries.as_ptr();
        let pair_table = self.pairs.as_ptr();
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
            // a pair lookup always stores two bytes, so it needs room for
            // two per lookup, or it would spill into the next segment
            while pairs && (0..4).all(|i| ip[i] >= floor[i] && left[i] >= 2 * BURST) {
                for _ in 0..BURST {
                    for i in 0..4 {
                        let entry = *pair_table.add((bits[i] >> shift) as usize);
                        op[i].cast::<u16>().write_unaligned((entry as u16).to_le());
                        op[i] = op[i].add((entry >> 24) as usize);
                        bits[i] <<= (entry >> 16) & 0xFF;
                    }
                }
                for i in 0..4 {
                    let used = bits[i].trailing_zeros();
                    ip[i] = ip[i].sub((used >> 3) as usize);
                    bits[i] = (load64(ip[i]) | 1) << (used & 7);
                    left[i] = lens[i] - op[i].offset_from(dst.add(lens[..i].iter().sum())) as usize;
                }
            }
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
