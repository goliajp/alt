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
        // is at least `first`, reps were limited to `reach` just above, and
        // the tables hold only indices of positions already passed (or
        // below `first`)
        let anchor = unsafe { self.find(window, start, end, low, &mut reps, store) };
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

    /// Runs the match finder over `start..end` and returns where the
    /// trailing literals begin.
    ///
    /// # Safety
    /// As for the `run` functions of the two finders, except that the
    /// prefix argument is derived here.
    unsafe fn find(
        &mut self,
        window: &Window<'_>,
        start: usize,
        end: usize,
        low: usize,
        reps: &mut [usize; 2],
        store: &mut SeqStore,
    ) -> usize {
        let prefix = window.has_prefix();
        // safety: forwarded from the caller; the const argument follows
        // `has_prefix`, and the special build runs only where detected
        unsafe {
            #[cfg(target_arch = "x86_64")]
            if crate::has_bmi2() {
                return if prefix {
                    self.find_bmi2::<true>(window, start, end, low, reps, store)
                } else {
                    self.find_bmi2::<false>(window, start, end, low, reps, store)
                };
            }
            if prefix {
                self.find_any::<true>(window, start, end, low, reps, store)
            } else {
                self.find_any::<false>(window, start, end, low, reps, store)
            }
        }
    }

    /// The finders compiled for processors with BMI2, where hashing and
    /// match measuring need no fixed shift register.
    ///
    /// # Safety
    /// As for [`Matcher::find_any`].
    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "bmi1,bmi2,lzcnt")]
    unsafe fn find_bmi2<const PREFIX: bool>(
        &mut self,
        window: &Window<'_>,
        start: usize,
        end: usize,
        low: usize,
        reps: &mut [usize; 2],
        store: &mut SeqStore,
    ) -> usize {
        // safety: forwarded from the caller
        unsafe { self.find_any::<PREFIX>(window, start, end, low, reps, store) }
    }

    /// # Safety
    /// As for the `run` functions of the two finders.
    #[inline(always)]
    unsafe fn find_any<const PREFIX: bool>(
        &mut self,
        window: &Window<'_>,
        start: usize,
        end: usize,
        low: usize,
        reps: &mut [usize; 2],
        store: &mut SeqStore,
    ) -> usize {
        // safety: forwarded from the caller
        unsafe {
            if self.params.double {
                let search = dfast::Search {
                    long: &mut *self.long,
                    long_log: self.params.long_log,
                    short: &mut *self.short,
                    short_log: self.params.short_log,
                    width: self.params.width,
                    low,
                };
                dfast::run::<PREFIX>(search, window, start, end, reps, store)
            } else {
                let search = fast::Search {
                    table: &mut *self.short,
                    log: self.params.short_log,
                    width: self.params.width,
                    low,
                };
                fast::run::<PREFIX>(search, window, start, end, reps, store)
            }
        }
    }
}
