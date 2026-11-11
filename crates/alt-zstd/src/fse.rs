//! Finite-state-entropy pieces shared by the decoder and the encoder: the
//! sequence code tables, the predefined distributions, the distribution
//! header and the decoding table.

use crate::Error;
use crate::bits::FwdReader;

pub(crate) const LL_MAX_LOG: u32 = 9;
pub(crate) const ML_MAX_LOG: u32 = 9;
pub(crate) const OF_MAX_LOG: u32 = 8;

pub(crate) const LL_SYMBOLS: usize = 36;
pub(crate) const ML_SYMBOLS: usize = 53;
pub(crate) const OF_SYMBOLS: usize = 32;
/// The largest alphabet among the three sequence fields.
pub(crate) const MAX_SYMBOLS: usize = ML_SYMBOLS;

pub(crate) static LL_BASE: [u32; LL_SYMBOLS] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 18, 20, 22, 24, 28, 32, 40, 48, 64,
    128, 256, 512, 1024, 2048, 4096, 8192, 16384, 32768, 65536,
];
pub(crate) static LL_EXTRA: [u8; LL_SYMBOLS] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 3, 3, 4, 6, 7, 8, 9, 10, 11,
    12, 13, 14, 15, 16,
];
pub(crate) static ML_BASE: [u32; ML_SYMBOLS] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27,
    28, 29, 30, 31, 32, 33, 34, 35, 37, 39, 41, 43, 47, 51, 59, 67, 83, 99, 131, 259, 515, 1027,
    2051, 4099, 8195, 16387, 32771, 65539,
];
pub(crate) static ML_EXTRA: [u8; ML_SYMBOLS] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    1, 1, 1, 1, 2, 2, 3, 3, 4, 4, 5, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16,
];

pub(crate) const LL_DEFAULT_LOG: u32 = 6;
pub(crate) const ML_DEFAULT_LOG: u32 = 6;
pub(crate) const OF_DEFAULT_LOG: u32 = 5;

pub(crate) static LL_DEFAULT: [i16; LL_SYMBOLS] = [
    4, 3, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 1, 1, 1, 2, 2, 2, 2, 2, 2, 2, 2, 2, 3, 2, 1, 1, 1, 1, 1,
    -1, -1, -1, -1,
];
pub(crate) static ML_DEFAULT: [i16; ML_SYMBOLS] = [
    1, 4, 3, 2, 2, 2, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, -1, -1, -1, -1, -1, -1, -1,
];
pub(crate) static OF_DEFAULT: [i16; 29] = [
    1, 1, 1, 1, 1, 1, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, -1, -1, -1, -1, -1,
];

/// What every entry of a code holds, whatever its state: the baseline and the
/// extra-bit count, in their places. Padded to 64 so that a masked index
/// needs no bounds check.
const fn fixed_parts(base: &[u32], extra: &[u8]) -> [u64; 64] {
    let mut out = [0u64; 64];
    let mut code = 0;
    while code < base.len() {
        out[code] = Entry::new(base[code], 0, extra[code], 0).0;
        code += 1;
    }
    out
}

pub(crate) static LL_FIXED: [u64; 64] = fixed_parts(&LL_BASE, &LL_EXTRA);
pub(crate) static ML_FIXED: [u64; 64] = fixed_parts(&ML_BASE, &ML_EXTRA);
pub(crate) static OF_FIXED: [u64; 64] = {
    // codes 0 and 1 are repeat offsets and never use the baseline; the
    // others stand for (1 << code) + bits - 3
    let mut out = [0u64; 64];
    let mut code = 0;
    while code < OF_SYMBOLS {
        out[code] = Entry::new((1u32 << code).wrapping_sub(3), 0, code as u8, 0).0;
        code += 1;
    }
    out
};

/// One state of a decoding table: the value it stands for and how to reach
/// the next state. Packed into one word so that a state costs one load.
#[derive(Clone, Copy, Default)]
pub(crate) struct Entry(u64);

impl Entry {
    pub(crate) const fn new(base: u32, next: u16, extra: u8, bits: u8) -> Self {
        Self((base as u64) << 32 | (next as u64) << 16 | (extra as u64) << 8 | bits as u64)
    }

    /// An entry from the fixed part of its code.
    #[inline(always)]
    pub(crate) fn with(fixed: u64, next: u16, bits: u32) -> Self {
        Self(fixed | (next as u64) << 16 | bits as u64)
    }

    /// Fresh bits to read for the next state.
    #[inline(always)]
    pub(crate) fn bits(self) -> u32 {
        self.0 as u8 as u32
    }

    /// Extra bits that follow the baseline in the stream.
    #[cfg(not(target_arch = "x86_64"))]
    #[inline(always)]
    pub(crate) fn extra(self) -> u32 {
        (self.0 >> 8) as u8 as u32
    }

    /// Next state before the fresh bits are added.
    #[inline(always)]
    pub(crate) fn next(self) -> usize {
        (self.0 >> 16) as u16 as usize
    }

    /// Baseline of the decoded value (the symbol itself for plain tables).
    #[inline(always)]
    pub(crate) fn base(self) -> usize {
        (self.0 >> 32) as usize
    }
}

/// Parses a distribution header into `norm`, which must be all zero and
/// whose length is the largest alphabet allowed. Returns the accuracy log,
/// the number of symbols described and the bytes consumed. On success the
/// absolute values in `norm` sum to exactly `1 << log`.
#[inline(always)]
pub(crate) fn read_distribution(
    src: &[u8],
    norm: &mut [i16],
    max_log: u32,
) -> Result<(u32, usize, usize), Error> {
    let mut reader = FwdReader::new(src);
    let log = (reader.peek() & 15) + 5;
    reader.skip(4);
    if log > max_log {
        return Err(Error::Corrupt);
    }
    let mut remaining = (1i32 << log) + 1;
    let mut threshold = 1i32 << log;
    let mut width = log + 1;
    let mut symbol = 0;
    let mut after_zero = false;
    while remaining > 1 && symbol < norm.len() {
        if after_zero {
            // two-bit run lengths of further zeros; 3 means another follows
            loop {
                let run = (reader.peek() & 3) as usize;
                reader.skip(2);
                symbol += run;
                if run != 3 || symbol >= norm.len() {
                    break;
                }
            }
            if symbol >= norm.len() {
                return Err(Error::Corrupt);
            }
        }
        let bits = reader.peek() as i32;
        let short_limit = 2 * threshold - 1 - remaining;
        let mut count = bits & (threshold - 1);
        if count < short_limit {
            reader.skip(width - 1);
        } else {
            count = bits & (2 * threshold - 1);
            if count >= threshold {
                count -= short_limit;
            }
            reader.skip(width);
        }
        // stored off by one so that -1 ("less than one") fits
        count -= 1;
        remaining -= count.abs();
        norm[symbol] = count as i16;
        symbol += 1;
        after_zero = count == 0;
        while remaining < threshold {
            width -= 1;
            threshold >>= 1;
        }
    }
    if remaining != 1 || reader.bytes_used() > src.len() {
        return Err(Error::Corrupt);
    }
    Ok((log, symbol, reader.bytes_used()))
}

/// States of the largest table.
pub(crate) const SLOTS: usize = 1 << LL_MAX_LOG;

/// Lays the symbols of a distribution out in the order in which the walk
/// over the table meets them, eight at a time, and lists the "less than
/// one" symbols. Returns how many of those there are.
#[inline(always)]
fn lay_out(norm: &[i16], ordered: &mut [u8; SLOTS + 8], lows: &mut [u8; 256]) -> usize {
    let mut low_count = 0;
    let mut filled = 0;
    for (symbol, &count) in norm.iter().enumerate() {
        // written whether or not it counts, to spare a branch
        lows[low_count % 256] = symbol as u8;
        low_count += (count == -1) as usize;
        let n = count.max(0) as usize;
        let pattern = [symbol as u8; 8];
        ordered[filled..filled + 8].copy_from_slice(&pattern);
        let mut more = 8;
        while more < n {
            ordered[filled + more..filled + more + 8].copy_from_slice(&pattern);
            more += 8;
        }
        filled += n;
    }
    low_count
}

fn walk_step(size: usize) -> usize {
    (size >> 1) + (size >> 3) + 3
}

/// Moves the laid-out symbols to their states. The walk takes every state
/// once, in an order that does not depend on the symbols; a state at the
/// top, reserved for a "less than one" symbol, takes a write too but uses
/// up no symbol, and gets its own symbol afterwards.
#[inline(always)]
fn scatter(ordered: &[u8; SLOTS + 8], lows: &[u8], log: u32, slots: &mut [u8; SLOTS]) {
    let size = 1usize << log;
    let high = size - lows.len();
    let (step, mask) = (walk_step(size), size - 1);
    let mut at = 0;
    let mut next = 0;
    for _ in 0..size {
        slots[at % SLOTS] = ordered[next % SLOTS];
        next += (at < high) as usize;
        at = (at + step) & mask;
    }
    for (slot, &symbol) in slots[..size].iter_mut().rev().zip(lows) {
        *slot = symbol;
    }
}

/// Lays the symbols of a distribution out over the `1 << log` states, for
/// the encoder. `norm` must sum (in absolute values) to the table size.
pub(crate) fn spread(norm: &[i16], log: u32, slots: &mut [u8]) {
    let size = 1usize << log;
    let mut high = size;
    for (symbol, &count) in norm.iter().enumerate() {
        if count == -1 {
            high -= 1;
            slots[high] = symbol as u8;
        }
    }
    let step = (size >> 1) + (size >> 3) + 3;
    let mask = size - 1;
    if high == size {
        // nothing is reserved, so the walk visits every slot once: lay the
        // symbols out in order, eight at a time, then scatter them
        let mut ordered = [0u8; (1 << LL_MAX_LOG) + 8];
        let mut filled = 0;
        for (symbol, &count) in norm.iter().enumerate() {
            let pattern = [symbol as u8; 8];
            for chunk in (0..count as usize).step_by(8) {
                ordered[filled + chunk..filled + chunk + 8].copy_from_slice(&pattern);
            }
            filled += count as usize;
        }
        let mut at = 0;
        for &symbol in &ordered[..size] {
            slots[at] = symbol;
            at = (at + step) & mask;
        }
        return;
    }
    let mut at = 0;
    for (symbol, &count) in norm.iter().enumerate() {
        for _ in 0..count.max(0) {
            slots[at] = symbol as u8;
            at = (at + step) & mask;
            // the top of the table belongs to the "less than one" symbols
            while at >= high {
                at = (at + step) & mask;
            }
        }
    }
}

/// Builds a decoding table from a valid distribution of at most `N`
/// symbols. `fixed` maps a symbol to the part of its entries that never
/// changes; `bias` is added to every next state, for tables that live at an
/// offset inside a larger array.
#[inline(always)]
pub(crate) fn build_table<const N: usize>(
    norm: &[i16],
    log: u32,
    table: &mut [Entry],
    bias: u16,
    fixed: impl Fn(usize) -> u64,
) {
    let size = 1usize << log;
    let table = &mut table[..size];
    // the state that the next entry of each symbol stands for
    let mut states = [0u16; N];
    for (state, &count) in states.iter_mut().zip(norm) {
        *state = count.unsigned_abs();
    }
    let rebase = (bias as u32).wrapping_sub(size as u32);
    let mut entry = |symbol: u8| {
        let state = &mut states[symbol as usize % N];
        let now = *state as u32;
        *state += 1;
        let bits = log - now.ilog2();
        let next = (now << bits).wrapping_add(rebase) as u16;
        Entry::with(fixed(symbol as usize), next, bits)
    };
    let mut ordered = [0u8; SLOTS + 8];
    let mut lows = [0u8; 256];
    let low_count = lay_out(norm, &mut ordered, &mut lows);
    if low_count == 0 {
        // with no state reserved the walk is a multiplication by the step,
        // so the symbol of a state is found by multiplying back; two
        // rounds of Newton's iteration give the inverse modulo 4096
        let step = walk_step(size);
        let mut inverse = step;
        for _ in 0..2 {
            inverse = inverse.wrapping_mul(2usize.wrapping_sub(step.wrapping_mul(inverse)));
        }
        for (at, cell) in table.iter_mut().enumerate() {
            *cell = entry(ordered[(at.wrapping_mul(inverse) & (size - 1)) % SLOTS]);
        }
    } else {
        let mut slots = [0u8; SLOTS];
        scatter(&ordered, &lows[..low_count], log, &mut slots);
        for (cell, &symbol) in table.iter_mut().zip(&slots[..size]) {
            *cell = entry(symbol);
        }
    }
}
