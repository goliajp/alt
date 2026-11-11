//! FSE encoding: distribution fitting, the distribution header, the
//! encoding table and cost estimates.

use std::sync::OnceLock;

use crate::bits::BitWriter;
use crate::fse::{LL_MAX_LOG, MAX_SYMBOLS, spread};

/// Encoding table for an alphabet of up to `MAX_SYMBOLS` symbols.
#[derive(Clone)]
pub(crate) struct CTable {
    /// next state, indexed through a symbol's `first_state`
    states: [u16; 1 << LL_MAX_LOG],
    /// added to the state, the top 16 bits give the bits to flush
    bit_cost: [u32; MAX_SYMBOLS],
    /// offset of a symbol's states in `states`, less its smallest state
    first_state: [i32; MAX_SYMBOLS],
    pub log: u32,
}

impl CTable {
    pub(crate) fn new() -> Self {
        Self {
            states: [0; 1 << LL_MAX_LOG],
            bit_cost: [0; MAX_SYMBOLS],
            first_state: [0; MAX_SYMBOLS],
            log: 0,
        }
    }

    /// Builds the table for a distribution that sums to `1 << log`.
    pub(crate) fn build(&mut self, norm: &[i16], log: u32) {
        let size = 1usize << log;
        let mut slots = [0u8; 1 << LL_MAX_LOG];
        spread(norm, log, &mut slots[..size]);
        let mut next = [0usize; MAX_SYMBOLS];
        let mut total = 0;
        for (symbol, &count) in norm.iter().enumerate() {
            next[symbol] = total;
            total += count.unsigned_abs() as usize;
        }
        for (state, &slot) in slots[..size].iter().enumerate() {
            let at = &mut next[slot as usize];
            self.states[*at] = (size + state) as u16;
            *at += 1;
        }
        let mut total = 0i32;
        for (symbol, &count) in norm.iter().enumerate() {
            let count = count.unsigned_abs() as u32;
            match count {
                0 => self.bit_cost[symbol] = ((log + 1) << 16) - (1 << log),
                1 => {
                    self.bit_cost[symbol] = (log << 16) - (1 << log);
                    self.first_state[symbol] = total - 1;
                }
                _ => {
                    let max_bits = log - (count - 1).ilog2();
                    self.bit_cost[symbol] = (max_bits << 16) - (count << max_bits);
                    self.first_state[symbol] = total - count as i32;
                }
            }
            total += count as i32;
        }
        self.log = log;
    }

    /// Builds the table of a stream that only ever holds `symbol`: no bits.
    pub(crate) fn build_single(&mut self, symbol: usize) {
        self.states[0] = 0;
        self.bit_cost[symbol] = 0;
        self.first_state[symbol] = 0;
        self.log = 0;
    }

    /// State for the first symbol encoded (the last one decoded), chosen so
    /// that the decoder needs fresh bits to leave it.
    #[inline(always)]
    pub(crate) fn start(&self, symbol: usize) -> u32 {
        let cost = self.bit_cost[symbol];
        let bits = (cost + (1 << 15)) >> 16;
        let value = (bits << 16) - cost;
        self.states[((value >> bits) as i32 + self.first_state[symbol]) as usize] as u32
    }

    /// Encodes `symbol`: flushes the low bits of `state` and moves on.
    #[inline(always)]
    pub(crate) fn encode(&self, writer: &mut BitWriter<'_>, state: &mut u32, symbol: usize) {
        let bits = (*state + self.bit_cost[symbol]) >> 16;
        writer.put((*state & ((1 << bits) - 1)) as u64, bits);
        let index = (*state >> bits) as i32 + self.first_state[symbol];
        *state = self.states[index as usize & ((1 << LL_MAX_LOG) - 1)] as u32;
    }

    /// The final state as the bits to write, which the decoder reads first.
    pub(crate) fn last(&self, state: u32) -> (u64, u32) {
        ((state & ((1 << self.log) - 1)) as u64, self.log)
    }
}

/// Accuracy log for `total` symbols over an alphabet whose largest symbol is
/// `max_symbol`.
pub(crate) fn table_log(max_log: u32, total: u32, max_symbol: usize) -> u32 {
    let by_total = (total - 1).max(1).ilog2().saturating_sub(2);
    let floor = (total.ilog2() + 1).min((max_symbol as u32).max(1).ilog2() + 2);
    max_log.min(by_total).max(floor).max(5)
}

/// Scales `counts` (sum `total`, at least two nonzero) to a distribution
/// summing to `1 << log`, keeping every present symbol at 1 or more.
pub(crate) fn normalize(counts: &[u32], total: u32, log: u32, norm: &mut [i16]) {
    let size = 1i32 << log;
    let mut sum = 0i32;
    for (slot, &count) in norm.iter_mut().zip(counts) {
        *slot = match count {
            0 => 0,
            _ => (((count as u64) << log) / total as u64).max(1) as i16,
        };
        sum += *slot as i32;
    }
    // each unit goes where it saves, or costs, the fewest bits: moving a
    // probability from p to p + 1 changes the cost by about count / (p + ½)
    while sum != size {
        let grow = sum < size;
        let mut best = usize::MAX;
        let mut best_key = if grow { f32::MIN } else { f32::MAX };
        for (symbol, (&count, &slot)) in counts.iter().zip(norm.iter()).enumerate() {
            if count == 0 || (!grow && slot < 2) {
                continue;
            }
            let key = count as f32 / (slot as f32 + if grow { 0.5 } else { -0.5 });
            if (grow && key > best_key) || (!grow && key < best_key) {
                best = symbol;
                best_key = key;
            }
        }
        if grow {
            norm[best] += 1;
            sum += 1;
        } else {
            norm[best] -= 1;
            sum -= 1;
        }
    }
}

/// Appends the header that describes `norm`, a distribution summing to
/// `1 << log`.
pub(crate) fn write_distribution(norm: &[i16], log: u32, out: &mut Vec<u8>) {
    let mut writer = BitWriter::new(out);
    writer.put((log - 5) as u64, 4);
    let mut remaining = (1i32 << log) + 1;
    let mut threshold = 1i32 << log;
    let mut width = log + 1;
    let mut symbol = 0;
    let mut after_zero = false;
    while remaining > 1 {
        if after_zero {
            let start = symbol;
            while norm[symbol] == 0 {
                symbol += 1;
            }
            let mut run = symbol - start;
            while run >= 3 {
                writer.put(3, 2);
                writer.flush();
                run -= 3;
            }
            writer.put(run as u64, 2);
        }
        let count = norm[symbol] as i32;
        symbol += 1;
        let short_limit = 2 * threshold - 1 - remaining;
        remaining -= count.abs();
        // stored off by one so that -1 ("less than one") fits
        let mut value = count + 1;
        if value >= threshold {
            value += short_limit;
        }
        writer.put(value as u64, width - (value < short_limit) as u32);
        writer.flush();
        after_zero = count == 0;
        while remaining < threshold {
            width -= 1;
            threshold >>= 1;
        }
    }
    writer.finish();
}

fn log2_of(value: u32) -> f32 {
    static TABLE: OnceLock<[f32; (1 << LL_MAX_LOG) + 1]> = OnceLock::new();
    TABLE.get_or_init(|| std::array::from_fn(|i| (i.max(1) as f32).log2()))[value as usize]
}

/// Bits needed to code `counts` with the distribution `norm` at accuracy
/// `log`, or `None` when a symbol that occurs has no probability.
pub(crate) fn cost(counts: &[u32], norm: &[i16], log: u32) -> Option<u32> {
    let mut bits = 0f32;
    for (symbol, &count) in counts.iter().enumerate() {
        if count == 0 {
            continue;
        }
        let share = norm.get(symbol).map_or(0, |n| n.unsigned_abs() as u32);
        if share == 0 {
            return None;
        }
        bits += count as f32 * (log as f32 - log2_of(share));
    }
    Some(bits as u32 + 1)
}
