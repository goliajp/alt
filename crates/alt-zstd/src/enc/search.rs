//! The match finder of one frame: its tables, the prefix index and the
//! per-block bookkeeping of repeat offsets.

use super::matcher::{PROBE, SeqStore, Window, hash};
use super::params::Params;
use super::{dfast, fast};

/// Hash tables and the two latest offsets, for one frame.
pub(crate) struct Matcher<'t> {
    pub params: Params,
    pub long: &'t mut [u32],
    pub short: &'t mut [u32],
    /// the two latest offsets as the decoder knows them
    pub reps: [usize; 2],
    pub max_distance: usize,
}

impl Matcher<'_> {
    /// Indexes the prefix of `window`. The tables may hold older entries as
    /// long as they are below `window.first`.
    pub(crate) fn load(&mut self, window: &Window<'_>) {
        let len = window.split - window.first;
        let table_log = if self.params.double {
            self.params.long_log
        } else {
            self.params.short_log
        };
        // sparse enough to leave the table half empty
        let step = (len >> (table_log - 1)).max(3);
        let drop = 64 - 8 * self.params.width;
        let mut index = window.first;
        while index + PROBE <= window.split {
            // safety: the 8 bytes at `index` lie inside the prefix
            let value = unsafe { window.prefix64(index) };
            if self.params.double {
                self.long[hash(value, 0, 64 - self.params.long_log)] = index as u32;
            }
            self.short[hash(value, drop, 64 - self.params.short_log)] = index as u32;
            index += step;
        }
    }

    /// Finds the matches of the block `start..end` and fills `store` with
    /// its sequences and literals.
    pub(crate) fn block(
        &mut self,
        window: &Window<'_>,
        start: usize,
        end: usize,
        store: &mut SeqStore,
    ) {
        store.start(end - start);
        let low = end.saturating_sub(self.max_distance).max(window.first);
        // an offset reaching below `low` is set aside for this block
        let reach = start - low;
        let before = self.reps;
        let mut reps = before.map(|rep| if rep > reach { 0 } else { rep });
        // safety: start..end is a block inside the data of `window`, `low`
        // is at least `first`, reps were limited to `reach` just above, the
        // tables hold only indices of positions already passed (or below
        // `first`), and the const argument follows `has_prefix`
        let anchor = unsafe {
            if self.params.double {
                let search = dfast::Search {
                    long: &mut *self.long,
                    long_log: self.params.long_log,
                    short: &mut *self.short,
                    short_log: self.params.short_log,
                    width: self.params.width,
                    low,
                };
                if window.has_prefix() {
                    dfast::run::<true>(search, window, start, end, &mut reps, store)
                } else {
                    dfast::run::<false>(search, window, start, end, &mut reps, store)
                }
            } else {
                let search = fast::Search {
                    table: &mut *self.short,
                    log: self.params.short_log,
                    width: self.params.width,
                    low,
                };
                if window.has_prefix() {
                    fast::run::<true>(search, window, start, end, &mut reps, store)
                } else {
                    fast::run::<false>(search, window, start, end, &mut reps, store)
                }
            }
        };
        // safety: anchor..end is the unmatched tail of the block
        let tail = unsafe { std::slice::from_raw_parts(window.data_at(anchor), end - anchor) };
        store.literals.extend_from_slice(tail);
        // the decoder still holds the offsets set aside above, the first of
        // them one place down if a new offset arrived
        let aside = before.map(|rep| if rep > reach { rep } else { 0 });
        let second = if aside[0] != 0 && reps[0] != 0 {
            aside[0]
        } else {
            aside[1]
        };
        self.reps = [
            if reps[0] != 0 { reps[0] } else { aside[0] },
            if reps[1] != 0 { reps[1] } else { second },
        ];
    }
}
