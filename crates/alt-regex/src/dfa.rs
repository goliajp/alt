//! Deciding whether and where a text matches with a deterministic
//! automaton whose states are built as the text first reaches them: a
//! table lookup per byte once they are. A state is the set of
//! instructions the paths of the nondeterministic automaton could be at.

use std::collections::HashMap;

use crate::program::{Inst, Program};
use crate::syntax::Look;

/// A state is named by where its row begins in the table. The first row
/// stands for a state not built yet, the second for the dead state.
const UNKNOWN: u32 = 0;
/// Set on the name of a state some path has matched in.
const MATCH: u32 = 1 << 31;
/// The table is thrown away and begun again once the states built take
/// more bytes than this.
const MAX_BYTES: usize = 2 << 20;
/// What a state takes besides its instructions and its row.
const STATE_BYTES: usize = 64;

pub struct Cache {
    /// Whether a match may begin at every position, or only where the
    /// search starts.
    anywhere: bool,
    names: HashMap<Box<[u32]>, u32>,
    /// The instructions of each state, in the order of their rows.
    states: Vec<Box<[u32]>>,
    /// A row per state: the state after it on each class of byte, and
    /// last what it comes to at the end of the text.
    table: Vec<u32>,
    /// The length of a row, which names the dead state.
    width: u32,
    /// What the states built so far take, about.
    bytes: usize,
    /// The state a search starts in: inside the text, and at its start.
    first: [u32; 2],
    /// Stamps instructions a closure has visited.
    visited: Vec<u32>,
    stamp: u32,
    stack: Vec<u32>,
}

impl Cache {
    pub fn new(anywhere: bool) -> Self {
        Cache {
            anywhere,
            names: HashMap::new(),
            states: Vec::new(),
            table: Vec::new(),
            width: 0,
            bytes: 0,
            first: [UNKNOWN; 2],
            visited: Vec::new(),
            stamp: 0,
            stack: Vec::new(),
        }
    }

    fn clear(&mut self) {
        self.names.clear();
        self.states.clear();
        self.table.clear();
        self.bytes = 0;
        self.first = [UNKNOWN; 2];
    }

    /// The state a search that starts at `from` begins in.
    fn start(&mut self, prog: &Program, from: usize) -> u32 {
        let at_start = from == 0;
        if self.first[usize::from(at_start)] == UNKNOWN {
            let set = self.closure(prog, &[prog.start], at_start, false);
            self.first[usize::from(at_start)] = self.name(prog, set);
        }
        self.first[usize::from(at_start)]
    }

    /// The instructions reachable from `seeds` without consuming a byte.
    fn closure(&mut self, prog: &Program, seeds: &[u32], at_start: bool, at_end: bool) -> Vec<u32> {
        self.visited.resize(prog.insts.len(), 0);
        self.stamp += 1;
        let mut out = Vec::new();
        self.stack.extend(seeds.iter().rev());
        while let Some(pc) = self.stack.pop() {
            if std::mem::replace(&mut self.visited[pc as usize], self.stamp) == self.stamp {
                continue;
            }
            match prog.insts[pc as usize] {
                Inst::Split(first, second) => self.stack.extend([second, first]),
                Inst::Save(_, next) | Inst::SaveOpt(_, next) => self.stack.push(next),
                Inst::Look(Look::TextStart, next) if at_start => self.stack.push(next),
                Inst::Look(Look::TextEnd, next) if at_end => self.stack.push(next),
                Inst::Look(Look::TextStart, _) | Inst::BackRef(..) => {}
                // waits in the state for the end of the text
                Inst::Look(..)
                | Inst::Set(..)
                | Inst::Byte(..)
                | Inst::Switch(..)
                | Inst::Match => out.push(pc),
            }
        }
        out.sort_unstable();
        out
    }

    /// The name of the state that is this set of instructions.
    fn name(&mut self, prog: &Program, set: Vec<u32>) -> u32 {
        if self.states.is_empty() {
            self.width = prog.members.len() as u32 + 1;
            self.states.extend([Box::default(), Box::default()]);
            self.table.resize(2 * self.width as usize, UNKNOWN);
        }
        if set.is_empty() {
            return self.width;
        }
        if let Some(&name) = self.names.get(set.as_slice()) {
            return name;
        }
        let matched = set
            .iter()
            .any(|&pc| matches!(prog.insts[pc as usize], Inst::Match));
        let name = self.table.len() as u32 | if matched { MATCH } else { 0 };
        let set: Box<[u32]> = set.into();
        // the instructions are kept twice, as the state and as its name
        self.bytes += STATE_BYTES + 4 * (2 * set.len() + self.width as usize);
        self.states.push(set.clone());
        self.table
            .resize(self.table.len() + self.width as usize, UNKNOWN);
        self.names.insert(set, name);
        name
    }

    fn set(&self, state: u32) -> &[u32] {
        &self.states[(state / self.width) as usize]
    }

    /// The state after `state` on a byte of class `class`, built the
    /// first time a text comes to it: kept out of the searches, whose
    /// loops then stay as small as they are written.
    #[cold]
    #[inline(never)]
    fn step(&mut self, prog: &Program, state: u32, class: usize) -> u32 {
        let byte = prog.members[class];
        let seeds: Vec<u32> = self
            .set(state)
            .iter()
            .filter_map(|&pc| match prog.insts[pc as usize] {
                Inst::Byte(b, next) if b == byte => Some(next),
                Inst::Set(set, next) if prog.sets[set as usize].contains(byte) => Some(next),
                Inst::Switch(copied, place) => prog.taken(copied, place, byte),
                _ => None,
            })
            .collect();
        let mut set = self.closure(prog, &seeds, false, false);
        if self.anywhere && !prog.anchored {
            // a match may begin at any later position
            set.extend(self.closure(prog, &[prog.start], false, false));
            set.sort_unstable();
            set.dedup();
        }
        if self.bytes >= MAX_BYTES {
            self.clear();
            return self.name(prog, set);
        }
        let next = self.name(prog, set);
        self.table[state as usize + class] = next;
        next
    }

    /// Whether `state`, reached at the end of the text, is a match there.
    #[inline]
    fn finish(&mut self, prog: &Program, state: u32) -> bool {
        let last = state as usize + self.width as usize - 1;
        if self.table[last] == UNKNOWN {
            self.settle(prog, state, last);
        }
        self.table[last] & MATCH != 0
    }

    /// Works out, once for a state, what the end of the text makes of it.
    #[cold]
    #[inline(never)]
    fn settle(&mut self, prog: &Program, state: u32, last: usize) {
        let seeds: Vec<u32> = self
            .set(state)
            .iter()
            .filter_map(|&pc| match prog.insts[pc as usize] {
                Inst::Look(Look::TextEnd, next) => Some(next),
                _ => None,
            })
            .collect();
        let set = self.closure(prog, &seeds, false, true);
        let matched = set
            .iter()
            .any(|&pc| matches!(prog.insts[pc as usize], Inst::Match));
        self.table[last] = self.width | if matched { MATCH } else { 0 };
    }

    /// Whether a state is one a search goes on from without a second
    /// look: built, alive and not a match.
    #[inline(always)]
    fn plain(&self, state: u32) -> bool {
        state.wrapping_sub(self.width + 1) < MATCH - self.width - 1
    }

    /// Follows the table from `state` over `text` from `at` for as long
    /// as the states are plain. Returns the position reached, the state
    /// there, and the entry that stopped the run: of the byte at that
    /// position, or nothing at the end of the text.
    #[inline(always)]
    fn run(
        &self,
        prog: &Program,
        text: &[u8],
        mut at: usize,
        mut state: u32,
    ) -> (usize, u32, Option<u32>) {
        let table = self.table.as_slice();
        let floor = self.width + 1;
        let plain = MATCH - floor;
        while let Some(&b) = text.get(at) {
            let next = table[state as usize + usize::from(prog.classes[usize::from(b)])];
            if next.wrapping_sub(floor) >= plain {
                return (at, state, Some(next));
            }
            state = next;
            at += 1;
        }
        (at, state, None)
    }
}

/// Where the first match to end in `text` ends, when the pattern
/// matches anywhere in it. The text is not empty, the program's only
/// conditions are on the ends of the text, and the cache is one for
/// matches that begin anywhere.
#[inline]
pub fn earliest_end(prog: &Program, cache: &mut Cache, text: &[u8]) -> Option<usize> {
    let mut state = cache.start(prog, 0);
    if !cache.plain(state) {
        return (state & MATCH != 0).then_some(0);
    }
    let mut at = 0;
    loop {
        let (reached, before, stopped) = cache.run(prog, text, at, state);
        let Some(mut next) = stopped else {
            return cache.finish(prog, before).then_some(text.len());
        };
        if next == UNKNOWN {
            let class = usize::from(prog.classes[usize::from(text[reached])]);
            next = cache.step(prog, before, class);
        }
        if next & MATCH != 0 {
            return Some(reached + 1);
        }
        if next == cache.width {
            return None;
        }
        (at, state) = (reached + 1, next);
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
    let mut state = cache.start(prog, from);
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
