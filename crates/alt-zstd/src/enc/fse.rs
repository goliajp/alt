//! FSE encoding: distribution fitting, the distribution header, the
//! encoding table and cost estimates.

use std::sync::OnceLock;

use crate::bits::BitWriter;
use crate::fse::{LL_MAX_LOG, MAX_SYMBOLS, spread};

/// Slots of the per-symbol table: a power of two above `MAX_SYMBOLS`, so
/// that a masked symbol always indexes inside it.
const SYMBOL_SLOTS: usize = 64;

/// How one symbol moves the state.
#[derive(Clone, Copy, Default)]
struct Step {
    /// added to the state, the top 16 bits give the bits to flush
    bit_cost: u32,
    /// offset of the symbol's states in `states`, less its smallest state
    first_state: i32,
}

/// Encoding table for an alphabet of up to `MAX_SYMBOLS` symbols.
#[derive(Clone)]
pub(crate) struct CTable {
    /// next state, indexed through a symbol's `first_state`
    states: [u16; 1 << LL_MAX_LOG],
    steps: [Step; SYMBOL_SLOTS],
    pub log: u32,
}

impl CTable {
    pub(crate) fn new() -> Self {
        Self {
            states: [0; 1 << LL_MAX_LOG],
            steps: [Step {
                bit_cost: 0,
                first_state: 0,
            }; SYMBOL_SLOTS],
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
        for (step, &count) in self.steps.iter_mut().zip(norm) {
            let count = count.unsigned_abs() as u32;
            match count {
                0 => step.bit_cost = ((log + 1) << 16) - (1 << log),
                1 => {
                    step.bit_cost = (log << 16) - (1 << log);
                    step.first_state = total - 1;
                }
                _ => {
                    let max_bits = log - (count - 1).ilog2();
                    step.bit_cost = (max_bits << 16) - (count << max_bits);
                    step.first_state = total - count as i32;
                }
            }
            total += count as i32;
        }
        self.log = log;
    }

    /// Builds the table of a stream that only ever holds `symbol`: no bits.
    pub(crate) fn build_single(&mut self, symbol: usize) {
        self.states[0] = 0;
        self.steps[symbol] = Step::default();
        self.log = 0;
    }

    /// State for the first symbol encoded (the last one decoded), chosen so
    /// that the decoder needs fresh bits to leave it.
    #[inline(always)]
    pub(crate) fn start(&self, symbol: usize) -> u32 {
        let step = self.steps[symbol % SYMBOL_SLOTS];
        let bits = (step.bit_cost + (1 << 15)) >> 16;
        let value = (bits << 16) - step.bit_cost;
        self.states[((value >> bits) as i32 + step.first_state) as usize] as u32
    }

    /// Encodes `symbol`: returns the low bits of `state` to write and how
    /// many they are, and moves the state on.
    #[inline(always)]
    pub(crate) fn encode(&self, state: &mut u32, symbol: usize) -> (u64, u32) {
        let step = self.steps[symbol % SYMBOL_SLOTS];
        let bits = (*state + step.bit_cost) >> 16;
        let low = *state & ((1 << bits) - 1);
        let index = (*state >> bits) as i32 + step.first_state;
        *state = self.states[index as usize % (1 << LL_MAX_LOG)] as u32;
        (low as u64, bits)
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

/// Fewest symbols from which rare ones get the "less than one" share.
const LOW_SHARE_MIN: u32 = 2048;

/// Scales `counts` (sum `total`, at least two nonzero) to a distribution
/// summing to `1 << log`, keeping every present symbol at 1 or more. In a
/// long stream a symbol too rare for a whole share gets -1, "less than
/// one": it still takes one state, but at the top of the table, out of the
/// way of the frequent symbols.
pub(crate) fn normalize(counts: &[u32], total: u32, log: u32, norm: &mut [i16]) {
    let size = 1i32 << log;
    let rare = if total >= LOW_SHARE_MIN {
        total >> log
    } else {
        0
    };
    let mut sum = 0i32;
    for (slot, &count) in norm.iter_mut().zip(counts) {
        *slot = match count {
            0 => 0,
            _ if count <= rare => -1,
            _ => (((count as u64) << log) / total as u64).max(1) as i16,
        };
        sum += slot.unsigned_abs() as i32;
    }
    if sum == size {
        return;
    }
    // each unit goes where it saves, or costs, the fewest bits: moving a
    // probability from p to p + 1 changes the cost by about count / (p + ½).
    // A symbol that may not move holds a key that never wins.
    let grow = sum < size;
    let (half, never) = if grow {
        (0.5, f32::MIN)
    } else {
        (-0.5, f32::MAX)
    };
    let key = |count: u32, slot: i16| {
        if count == 0 || slot < 0 || (!grow && slot < 2) {
            never
        } else {
            count as f32 / (slot as f32 + half)
        }
    };
    let mut keys = [never; MAX_SYMBOLS];
    for (key_slot, (&count, &slot)) in keys.iter_mut().zip(counts.iter().zip(norm.iter())) {
        *key_slot = key(count, slot);
    }
    let keys = &mut keys[..counts.len()];
    while sum != size {
        let mut best = 0;
        for (symbol, &candidate) in keys.iter().enumerate() {
            if (grow && candidate > keys[best]) || (!grow && candidate < keys[best]) {
                best = symbol;
            }
        }
        let unit = if grow { 1 } else { -1 };
        norm[best] += unit;
        sum += unit as i32;
        keys[best] = key(counts[best], norm[best]);
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
