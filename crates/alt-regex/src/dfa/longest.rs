//! The longest match from one place.

use super::{Cache, MATCH, UNKNOWN};
use crate::program::Program;

impl Cache {
    /// The state of a search that begins at the instruction `entry`,
    /// inside the text: a cache so begun serves that instruction only.
    fn entry(&mut self, prog: &Program, entry: u32) -> u32 {
        if self.first[0] == UNKNOWN {
            let set = self.closure(prog, &[entry], false, false);
            self.first[0] = self.name(prog, set);
        }
        self.first[0]
    }
}

/// Where the longest match that begins exactly at `from` ends, with a
/// cache for matches that begin only where the search starts. `work` is
/// counted down by the bytes looked at.
pub fn longest(
    prog: &Program,
    cache: &mut Cache,
    text: &[u8],
    from: usize,
    work: &mut usize,
) -> Option<usize> {
    let state = cache.start(prog, from);
    onward(prog, cache, text, (state, from), work)
}

/// As [`longest`], of what the program matches from its instruction
/// `entry` on, at `from` behind the start of the text: where a match
/// ends whose beginning was found otherwise.
pub fn longest_behind(
    prog: &Program,
    cache: &mut Cache,
    text: &[u8],
    (entry, from): (u32, usize),
    work: &mut usize,
) -> Option<usize> {
    let state = cache.entry(prog, entry);
    onward(prog, cache, text, (state, from), work)
}

#[inline(always)]
fn onward(
    prog: &Program,
    cache: &mut Cache,
    text: &[u8],
    (mut state, from): (u32, usize),
    work: &mut usize,
) -> Option<usize> {
    let mut last = None;
    if !cache.plain(state) {
        if state & MATCH == 0 {
            return None;
        }
        last = Some(from);
        state &= !MATCH;
    }
    let mut at = from;
    loop {
        let (reached, before, stopped) = cache.run(prog, text, at, state);
        let Some(mut next) = stopped else {
            *work = work.saturating_sub(reached - from);
            return match cache.finish(prog, before) {
                true => Some(text.len()),
                false => last,
            };
        };
        if next == UNKNOWN {
            let class = usize::from(prog.classes[usize::from(text[reached])]);
            next = cache.step(prog, before, class);
        }
        if next == cache.width {
            *work = work.saturating_sub(reached + 1 - from);
            return last;
        }
        if next & MATCH != 0 {
            last = Some(reached + 1);
            next &= !MATCH;
        }
        (at, state) = (reached + 1, next);
    }
}
