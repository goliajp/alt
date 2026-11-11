//! Single-table greedy match finder that probes positions in pairs: the
//! variant of the fast strength for inputs of more than one block.

use super::fast::Search;
use super::matcher::{PROBE, SKIP_SPAN, SeqStore, Window, hash};

/// Finds matches in `start..end` and returns the index where the trailing
/// literals begin. `reps` holds the two latest offsets, zero when unusable.
///
/// Positions are probed in adjacent pairs, with the table lookup of each
/// issued one position ahead of its use; the repeat offset is tried once
/// per pair.
///
/// # Safety
/// `window.split <= start <= end <= window.end`, `search.low >= window.first`,
/// every nonzero rep at most `start - search.low`, the table at least
/// `1 << search.log` entries long with all entries below `start`, `PREFIX`
/// true if the window has a prefix, and `store` started for this block.
#[inline(always)]
pub(crate) unsafe fn run<const PREFIX: bool>(
    search: Search<'_>,
    window: &Window<'_>,
    start: usize,
    end: usize,
    reps: &mut [usize; 2],
    store: &mut SeqStore,
) -> usize {
    if end - start < PROBE + 4 {
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
    // a position past `limit` cannot be hashed
    let limit = end - PROBE;
    let [mut rep0, mut rep1] = *reps;
    let mut anchor = start;
    let mut ip0 = start;

    // safety (whole block): `ip <= last` before every 8-byte read of the
    // data; candidates come from the table, so they are below `ip`, and
    // `probe` yields a readable address for any of them; hashes are below
    // 1 << log
    unsafe {
        let slot_of = |ip: usize| table.add(hash(window.data64(ip), drop, shift));
        let hit = |ip: usize, candidate: usize| {
            window.hits32_likely::<PREFIX>(candidate, low, window.data64(ip) as u32)
        };
        'search: loop {
            // the gap between pairs; pairs are (ip0, ip1) then (ip2, ip3)
            let mut step = 2;
            let mut next_step = ip0 + SKIP_SPAN / 2;
            let mut ip1 = ip0 + 1;
            let mut ip2 = ip0 + step;
            let mut ip3 = ip2 + 1;
            if ip3 >= limit {
                break;
            }
            let mut slot0 = slot_of(ip0);
            let mut slot1 = slot_of(ip1);
            let mut candidate = *slot0 as usize;
            // last position whose slot was written
            let mut indexed;
            let mut from;
            let mut match_len;
            let code;
            loop {
                let repeat = window.repeats::<PREFIX>(ip2, rep0);
                *slot0 = ip0 as u32;
                indexed = ip0;
                if repeat {
                    *slot1 = ip1 as u32;
                    ip0 = ip2;
                    from = ip0 - rep0;
                    // ip2 is at least two past the anchor, so one step back
                    // still leaves a literal in front of the match
                    let back =
                        (window.byte::<false>(ip0 - 1) == window.byte::<PREFIX>(from - 1)) as usize;
                    ip0 -= back;
                    from -= back;
                    match_len = 4 + back;
                    code = 1;
                    break;
                }
                if hit(ip0, candidate) {
                    *slot1 = ip1 as u32;
                } else {
                    candidate = *slot1 as usize;
                    slot0 = slot1;
                    slot1 = slot_of(ip2);
                    ip0 = ip1;
                    ip1 = ip2;
                    ip2 = ip3;
                    *slot0 = ip0 as u32;
                    indexed = ip0;
                    if hit(ip0, candidate) {
                        // only if it stays below where the search resumes
                        if step <= 4 {
                            *slot1 = ip1 as u32;
                        }
                    } else {
                        candidate = *slot1 as usize;
                        slot0 = slot1;
                        slot1 = slot_of(ip2);
                        ip0 = ip1;
                        ip1 = ip2;
                        ip2 = ip0 + step;
                        ip3 = ip1 + step;
                        if ip2 >= next_step {
                            step += 1;
                            next_step += SKIP_SPAN / 2;
                        }
                        if ip3 >= limit {
                            break 'search;
                        }
                        continue;
                    }
                }
                from = candidate;
                match_len = 4 + window.extend_back::<PREFIX>(&mut ip0, &mut from, anchor, low);
                rep1 = rep0;
                rep0 = ip0 - from;
                code = rep0 + 3;
                break;
            }
            match_len += window.common::<PREFIX>(ip0 + match_len, from + match_len, end);
            store.push(window, anchor, ip0, match_len, code);
            ip0 += match_len;
            anchor = ip0;

            if ip0 <= limit {
                // index two positions the match jumped over
                *slot_of(indexed + 2) = (indexed + 2) as u32;
                *slot_of(ip0 - 2) = (ip0 - 2) as u32;
                // a match right behind another often repeats the older offset
                while ip0 <= limit && window.repeats::<PREFIX>(ip0, rep1) {
                    let len = 4 + window.common::<PREFIX>(ip0 + 4, ip0 + 4 - rep1, end);
                    std::mem::swap(&mut rep0, &mut rep1);
                    *slot_of(ip0) = ip0 as u32;
                    store.push(window, ip0, ip0, len, 1);
                    ip0 += len;
                    anchor = ip0;
                }
            }
        }
    }
    *reps = [rep0, rep1];
    anchor
}
