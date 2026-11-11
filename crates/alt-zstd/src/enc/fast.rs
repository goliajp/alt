//! Single-table greedy match finder (the fast strength).

use super::matcher::{PROBE, SKIP_LOG, SeqStore, Window, hash};
use crate::bits::load32;

pub(crate) struct Search<'t> {
    pub table: &'t mut [u32],
    /// log2 of the table size
    pub log: u32,
    /// bytes hashed per position
    pub width: u32,
    /// lowest index a match may start at
    pub low: usize,
}

/// Finds matches in `start..end` and returns the index where the trailing
/// literals begin. `reps` holds the two latest offsets, zero when unusable.
///
/// # Safety
/// `window.split <= start <= end <= window.end`, `search.low >= window.first`,
/// every nonzero rep at most `start - search.low`, the table at least
/// `1 << search.log` entries long with all entries below `start`, `PREFIX`
/// true if the window has a prefix, and `store` started for this block.
pub(crate) unsafe fn run<const PREFIX: bool>(
    search: Search<'_>,
    window: &Window<'_>,
    start: usize,
    end: usize,
    reps: &mut [usize; 2],
    store: &mut SeqStore,
) -> usize {
    if end - start < PROBE + 1 {
        return start;
    }
    let Search {
        table,
        log,
        width,
        low,
    } = search;
    let table = table.as_mut_ptr();
    let drop = 64 - 8 * width;
    let shift = 64 - log;
    // a position past `last` cannot be hashed
    let last = end - PROBE;
    let [mut rep0, mut rep1] = *reps;
    let mut anchor = start;
    let mut ip = start;

    // safety (whole block): `ip <= last` before every 8-byte read of the
    // data; candidates come from the table, so they are below `ip`, and
    // `probe` yields a readable address for any of them; hashes are below
    // 1 << log
    unsafe {
        while ip <= last {
            let here = window.data64(ip);
            let slot = table.add(hash(here, drop, shift));
            let candidate = *slot as usize;
            *slot = ip as u32;
            let found = ip;
            let match_len;

            let (from_ptr, usable) = window.probe::<PREFIX>(candidate, low);
            if window.repeats::<PREFIX>(ip + 1, rep0) {
                ip += 1;
                match_len = 4 + window.common::<PREFIX>(ip + 4, ip + 4 - rep0, end);
                store.push(window, anchor, ip, match_len, 1);
            } else if (load32(from_ptr) == here as u32) & usable {
                let mut from = candidate;
                let forward = 4 + window.common::<PREFIX>(ip + 4, from + 4, end);
                match_len = forward + window.extend_back::<PREFIX>(&mut ip, &mut from, anchor, low);
                rep1 = rep0;
                rep0 = ip - from;
                store.push(window, anchor, ip, match_len, rep0 + 3);
            } else {
                ip += 1 + ((ip - anchor) >> SKIP_LOG);
                continue;
            }
            ip += match_len;
            anchor = ip;

            if ip <= last {
                // index two positions the match jumped over
                *table.add(hash(window.data64(found + 2), drop, shift)) = (found + 2) as u32;
                *table.add(hash(window.data64(ip - 2), drop, shift)) = (ip - 2) as u32;
                // a match right behind another often repeats the older offset
                while ip <= last && window.repeats::<PREFIX>(ip, rep1) {
                    let len = 4 + window.common::<PREFIX>(ip + 4, ip + 4 - rep1, end);
                    std::mem::swap(&mut rep0, &mut rep1);
                    *table.add(hash(window.data64(ip), drop, shift)) = ip as u32;
                    store.push(window, ip, ip, len, 1);
                    ip += len;
                    anchor = ip;
                }
            }
        }
    }
    *reps = [rep0, rep1];
    anchor
}
