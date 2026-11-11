//! Single-table greedy match finder (the fast strength). Positions are
//! probed in pairs, with the table lookup of each issued one position
//! ahead of its use.

use std::hint::select_unpredictable;

use super::matcher::{NOWHERE, PROBE, SKIP_SPAN, SeqStore, Window, hash, multiplier};
use crate::bits::{load32, load64};

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
/// Positions are probed in adjacent pairs, with the table lookup of each
/// issued one position ahead of its use; the repeat offset is tried once
/// per pair.
///
/// # Safety
/// `window.split <= start <= end <= window.end`, `search.low >= window.first`,
/// every nonzero rep at most `start - search.low`, the table at least
/// `1 << search.log` entries long with all entries below `start`, `PREFIX`
/// true if the window has a prefix, `LIKELY` set only where candidates are
/// rarely unusable (a long input), and `store` started for this block.
#[inline(always)]
pub(crate) unsafe fn run<const PREFIX: bool, const LIKELY: bool>(
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
    // safety: forwarded from the caller. Both forms find the same matches;
    // which one is faster differs by architecture
    unsafe {
        if !PREFIX && cfg!(target_arch = "aarch64") {
            run_plain::<LIKELY>(search, window, start, end, reps, store)
        } else {
            run_window::<PREFIX, LIKELY>(search, window, start, end, reps, store)
        }
    }
}

/// [`run`] with positions as indices and every access through the window,
/// which is what a prefix needs.
///
/// # Safety
/// As for [`run`], with at least `PROBE + 4` bytes in `start..end`.
#[inline(always)]
unsafe fn run_window<const PREFIX: bool, const LIKELY: bool>(
    search: Search<'_>,
    window: &Window<'_>,
    start: usize,
    end: usize,
    reps: &mut [usize; 2],
    store: &mut SeqStore,
) -> usize {
    let Search {
        table,
        log,
        width,
        low,
    } = search;
    let table = table.as_mut_ptr();
    let mix = multiplier(width);
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
        let slot_of = |ip: usize| table.add(hash(window.data64(ip), mix, shift));
        let hit = |ip: usize, candidate: usize| {
            let here = window.data64(ip) as u32;
            if LIKELY {
                window.hits32_likely::<PREFIX>(candidate, low, here)
            } else {
                window.hits32_unlikely::<PREFIX>(candidate, low, here)
            }
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

/// [`run`] for a window without a prefix, with positions kept as addresses
/// and table slots as hashes. It finds the same matches as [`run_window`].
///
/// # Safety
/// As for [`run`], with no prefix in the window and at least `PROBE + 4`
/// bytes in `start..end`.
#[inline(always)]
unsafe fn run_plain<const LIKELY: bool>(
    search: Search<'_>,
    window: &Window<'_>,
    start: usize,
    end: usize,
    reps: &mut [usize; 2],
    store: &mut SeqStore,
) -> usize {
    let Search {
        table,
        log,
        width,
        low,
    } = search;
    let table = table.as_mut_ptr();
    let mix = multiplier(width);
    let shift = 64 - log;
    // address of index 0; only indices from `low` on are read through it
    let base = window.data_at(0);
    let lowest = window.data_at(low);
    // a position past `limit` cannot be hashed
    let limit = window.data_at(end - PROBE);
    let [mut rep0, mut rep1] = *reps;
    let mut anchor = window.data_at(start);
    let mut ip0 = anchor;

    // safety (whole block): every position read lies in `start..=limit`,
    // so it has 8 bytes ahead of it; candidates come from the table, so
    // they are below the position probed, and one below `low` is not read;
    // a repeat offset reaches no further back than `low`; hashes are below
    // 1 << log
    unsafe {
        let index_of = |at: *const u8| at.offset_from(base) as usize;
        let hash_at = |at: *const u8| hash(load64(at), mix, shift);
        let hit = |at: *const u8, candidate: u32| {
            let usable = candidate as usize >= low;
            if LIKELY {
                usable && load32(base.add(candidate as usize)) == load32(at)
            } else {
                let from = base.wrapping_add(candidate as usize);
                let from = select_unpredictable(usable, from, NOWHERE[0].as_ptr());
                ((load32(from) ^ load32(at)) | !usable as u32) == 0
            }
        };
        'search: loop {
            // the gap between pairs; pairs are (ip0, ip1) then (ip2, ip3)
            let mut step = 2;
            let mut next_step = ip0.add(SKIP_SPAN / 2);
            let mut ip1 = ip0.add(1);
            let mut ip2 = ip0.add(step);
            let mut ip3 = ip2.add(1);
            if ip3 >= limit {
                break;
            }
            let mut hash0 = hash_at(ip0);
            let mut hash1 = hash_at(ip1);
            let mut candidate = *table.add(hash0);
            // last position whose slot was written, as an index
            let mut indexed;
            let mut from;
            let mut match_len;
            let code;
            loop {
                let repeated = load32(ip2.sub(rep0));
                indexed = index_of(ip0);
                *table.add(hash0) = indexed as u32;
                if (load32(ip2) == repeated) & (rep0 != 0) {
                    *table.add(hash1) = index_of(ip1) as u32;
                    ip0 = ip2;
                    from = ip0.sub(rep0);
                    // ip2 is at least two past the anchor, so one step back
                    // still leaves a literal in front of the match
                    let back = (ip0.sub(1).read() == from.sub(1).read()) as usize;
                    ip0 = ip0.sub(back);
                    from = from.sub(back);
                    match_len = 4 + back;
                    code = 1;
                    break;
                }
                if hit(ip0, candidate) {
                    *table.add(hash1) = index_of(ip1) as u32;
                } else {
                    candidate = *table.add(hash1);
                    hash0 = hash1;
                    hash1 = hash_at(ip2);
                    ip0 = ip1;
                    ip1 = ip2;
                    ip2 = ip3;
                    indexed = index_of(ip0);
                    *table.add(hash0) = indexed as u32;
                    if hit(ip0, candidate) {
                        // only if it stays below where the search resumes
                        if step <= 4 {
                            *table.add(hash1) = index_of(ip1) as u32;
                        }
                    } else {
                        candidate = *table.add(hash1);
                        hash0 = hash1;
                        hash1 = hash_at(ip2);
                        ip0 = ip1;
                        ip1 = ip2;
                        ip2 = ip0.add(step);
                        ip3 = ip1.add(step);
                        if ip2 >= next_step {
                            step += 1;
                            next_step = next_step.add(SKIP_SPAN / 2);
                        }
                        if ip3 >= limit {
                            break 'search;
                        }
                        continue;
                    }
                }
                from = base.add(candidate as usize);
                match_len = 4;
                while ip0 > anchor && from > lowest && ip0.sub(1).read() == from.sub(1).read() {
                    ip0 = ip0.sub(1);
                    from = from.sub(1);
                    match_len += 1;
                }
                rep1 = rep0;
                rep0 = ip0.offset_from(from) as usize;
                code = rep0 + 3;
                break;
            }
            let at = index_of(ip0);
            match_len += window.common::<false>(at + match_len, index_of(from) + match_len, end);
            store.push(window, index_of(anchor), at, match_len, code);
            ip0 = ip0.add(match_len);
            anchor = ip0;

            if ip0 <= limit {
                // index two positions the match jumped over
                *table.add(hash_at(base.add(indexed + 2))) = (indexed + 2) as u32;
                *table.add(hash_at(ip0.sub(2))) = index_of(ip0.sub(2)) as u32;
                // a match right behind another often repeats the older offset
                while ip0 <= limit && rep1 != 0 && load32(ip0) == load32(ip0.sub(rep1)) {
                    let at = index_of(ip0);
                    let len = 4 + window.common::<false>(at + 4, at + 4 - rep1, end);
                    std::mem::swap(&mut rep0, &mut rep1);
                    *table.add(hash_at(ip0)) = at as u32;
                    store.push(window, at, at, len, 1);
                    ip0 = ip0.add(len);
                    anchor = ip0;
                }
            }
        }
        *reps = [rep0, rep1];
        index_of(anchor)
    }
}
