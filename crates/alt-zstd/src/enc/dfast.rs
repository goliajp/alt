//! Two-table greedy match finder (the default strength): a table of 8-byte
//! hashes finds long matches, a table of shorter hashes is the fallback.

use super::matcher::{PROBE, SKIP_LOG, SeqStore, Window, hash};
use crate::bits::{load32, load64};

pub(crate) struct Search<'t> {
    pub long: &'t mut [u32],
    pub long_log: u32,
    pub short: &'t mut [u32],
    pub short_log: u32,
    /// bytes hashed per position for the short table
    pub width: u32,
    /// lowest index a match may start at
    pub low: usize,
}

/// Finds matches in `start..end` and returns the index where the trailing
/// literals begin. `reps` holds the two latest offsets, zero when unusable.
///
/// # Safety
/// `window.split <= start <= end <= window.end`, `search.low >= window.first`,
/// every nonzero rep at most `start - search.low`, each table at least
/// `1 << log` entries long with all entries below `start`, `PREFIX` true if
/// the window has a prefix, and `store` started for this block.
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
    let low = search.low;
    let long = search.long.as_mut_ptr();
    let short = search.short.as_mut_ptr();
    let long_shift = 64 - search.long_log;
    let drop = 64 - 8 * search.width;
    let short_shift = 64 - search.short_log;
    // a position past `last` cannot be hashed
    let last = end - PROBE;
    let [mut rep0, mut rep1] = *reps;
    let mut anchor = start;
    let mut ip = start;

    // safety (whole block): `ip < last` before the reads at ip and ip + 1;
    // candidates come from the tables, so they are below `ip`, and `probe`
    // yields a readable address for any of them; hashes are below 1 << log
    unsafe {
        while ip < last {
            let here = window.data64(ip);
            let long_slot = long.add(hash(here, 0, long_shift));
            let short_slot = short.add(hash(here, drop, short_shift));
            let long_candidate = *long_slot as usize;
            let short_candidate = *short_slot as usize;
            *long_slot = ip as u32;
            *short_slot = ip as u32;
            let found = ip;
            let match_len;

            let (long_ptr, long_usable) = window.probe::<PREFIX>(long_candidate, low);
            let (short_ptr, short_usable) = window.probe::<PREFIX>(short_candidate, low);
            if window.repeats::<PREFIX>(ip + 1, rep0) {
                ip += 1;
                match_len = 4 + window.common::<PREFIX>(ip + 4, ip + 4 - rep0, end);
                store.push(window, anchor, ip, match_len, 1);
            } else {
                let mut from;
                let forward;
                if (load64(long_ptr) == here) & long_usable {
                    from = long_candidate;
                    forward = 8 + window.common::<PREFIX>(ip + 8, from + 8, end);
                } else if (load32(short_ptr) == here as u32) & short_usable {
                    // a long match one byte on beats the short one here
                    let ahead = window.data64(ip + 1);
                    let slot = long.add(hash(ahead, 0, long_shift));
                    let next = *slot as usize;
                    *slot = (ip + 1) as u32;
                    let (next_ptr, next_usable) = window.probe::<PREFIX>(next, low);
                    if (load64(next_ptr) == ahead) & next_usable {
                        ip += 1;
                        from = next;
                        forward = 8 + window.common::<PREFIX>(ip + 8, from + 8, end);
                    } else {
                        from = short_candidate;
                        forward = 4 + window.common::<PREFIX>(ip + 4, from + 4, end);
                    }
                } else {
                    ip += 1 + ((ip - anchor) >> SKIP_LOG);
                    continue;
                }
                match_len = forward + window.extend_back::<PREFIX>(&mut ip, &mut from, anchor, low);
                rep1 = rep0;
                rep0 = ip - from;
                store.push(window, anchor, ip, match_len, rep0 + 3);
            }
            ip += match_len;
            anchor = ip;

            if ip <= last {
                // index positions the match jumped over
                let skipped = window.data64(found + 2);
                *long.add(hash(skipped, 0, long_shift)) = (found + 2) as u32;
                *short.add(hash(skipped, drop, short_shift)) = (found + 2) as u32;
                *long.add(hash(window.data64(ip - 2), 0, long_shift)) = (ip - 2) as u32;
                *short.add(hash(window.data64(ip - 1), drop, short_shift)) = (ip - 1) as u32;
                // a match right behind another often repeats the older offset
                while ip <= last && window.repeats::<PREFIX>(ip, rep1) {
                    let len = 4 + window.common::<PREFIX>(ip + 4, ip + 4 - rep1, end);
                    std::mem::swap(&mut rep0, &mut rep1);
                    let here = window.data64(ip);
                    *long.add(hash(here, 0, long_shift)) = ip as u32;
                    *short.add(hash(here, drop, short_shift)) = ip as u32;
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
