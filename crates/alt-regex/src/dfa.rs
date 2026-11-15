//! Deciding whether a text matches with a deterministic automaton whose
//! states are built as the text first reaches them: a table lookup per
//! byte once they are. A state is the set of instructions the paths of
//! the nondeterministic automaton could be at.

use std::collections::HashMap;

use crate::program::{Inst, Program};
use crate::syntax::Look;

const UNKNOWN: u32 = 0;
const DEAD: u32 = 1;
/// Set on the number of a state some path has matched in.
const MATCH: u32 = 1 << 31;
/// The table is thrown away and begun again past this many states.
const MAX_STATES: usize = 10_000;

pub struct Cache {
    /// Whether a match may begin at every position, or only where the
    /// search starts.
    anywhere: bool,
    ids: HashMap<Box<[u32]>, u32>,
    /// The instructions of each state; the first two stand for the
    /// unknown and the dead state.
    states: Vec<Box<[u32]>>,
    /// The state after each state on each class of byte, and at the end
    /// of the text.
    table: Vec<u32>,
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
            ids: HashMap::new(),
            states: Vec::new(),
            table: Vec::new(),
            first: [UNKNOWN; 2],
            visited: Vec::new(),
            stamp: 0,
            stack: Vec::new(),
        }
    }

    fn clear(&mut self) {
        self.ids.clear();
        self.states.clear();
        self.table.clear();
        self.first = [UNKNOWN; 2];
    }

    /// The state a search that starts at `from` begins in.
    fn start(&mut self, prog: &Program, from: usize) -> u32 {
        let at_start = from == 0;
        if self.first[usize::from(at_start)] == UNKNOWN {
            let set = self.closure(prog, &[prog.start], at_start, false);
            self.first[usize::from(at_start)] = self.intern(prog, set);
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
                Inst::Save(_, next) => self.stack.push(next),
                Inst::Look(Look::TextStart, next) if at_start => self.stack.push(next),
                Inst::Look(Look::TextEnd, next) if at_end => self.stack.push(next),
                Inst::Look(Look::TextStart, _) => {}
                // waits in the state for the end of the text
                Inst::Look(..) | Inst::Set(..) | Inst::Byte(..) | Inst::Match => out.push(pc),
            }
        }
        out.sort_unstable();
        out
    }

    fn intern(&mut self, prog: &Program, set: Vec<u32>) -> u32 {
        if set.is_empty() {
            return DEAD;
        }
        if let Some(&id) = self.ids.get(set.as_slice()) {
            return id;
        }
        if self.states.is_empty() {
            self.states.extend([Box::default(), Box::default()]);
            self.table.resize(2 * (prog.members.len() + 1), UNKNOWN);
        }
        let matched = set
            .iter()
            .any(|&pc| matches!(prog.insts[pc as usize], Inst::Match));
        let id = self.states.len() as u32 | if matched { MATCH } else { 0 };
        let set: Box<[u32]> = set.into();
        self.states.push(set.clone());
        self.table
            .resize(self.table.len() + prog.members.len() + 1, UNKNOWN);
        self.ids.insert(set, id);
        id
    }

    /// The state after `state` on a byte of class `class`.
    fn step(&mut self, prog: &Program, state: u32, class: usize) -> u32 {
        let byte = prog.members[class];
        let seeds: Vec<u32> = self.states[(state & !MATCH) as usize]
            .iter()
            .filter_map(|&pc| match prog.insts[pc as usize] {
                Inst::Byte(b, next) if b == byte => Some(next),
                Inst::Set(set, next) if prog.sets[set as usize].contains(byte) => Some(next),
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
        if self.states.len() >= MAX_STATES {
            self.clear();
            return self.intern(prog, set);
        }
        let next = self.intern(prog, set);
        let state = (state & !MATCH) as usize;
        self.table[state * (prog.members.len() + 1) + class] = next;
        next
    }

    /// Whether `state`, reached at the end of the text, is a match there.
    fn finish(&mut self, prog: &Program, state: u32) -> u32 {
        let seeds: Vec<u32> = self.states[(state & !MATCH) as usize]
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
        let next = if matched { DEAD | MATCH } else { DEAD };
        let width = prog.members.len() + 1;
        self.table[(state & !MATCH) as usize * width + width - 1] = next;
        next
    }
}

/// Where the first match to end in `text` ends, when the pattern
/// matches anywhere in it. The text is not empty, the program's only
/// conditions are on the ends of the text, and the cache is one for
/// matches that begin anywhere.
pub fn earliest_end(prog: &Program, cache: &mut Cache, text: &[u8]) -> Option<usize> {
    let width = prog.members.len() + 1;
    let mut state = cache.start(prog, 0);
    for (at, &b) in text.iter().enumerate() {
        if state & MATCH != 0 {
            return Some(at);
        }
        if state == DEAD {
            return None;
        }
        let class = usize::from(prog.classes[usize::from(b)]);
        state = match cache.table[state as usize * width + class] {
            UNKNOWN => cache.step(prog, state, class),
            known => known,
        };
    }
    if state & MATCH != 0 {
        return Some(text.len());
    }
    if state == DEAD {
        return None;
    }
    let end = match cache.table[state as usize * width + width - 1] {
        UNKNOWN => cache.finish(prog, state),
        known => known,
    };
    (end & MATCH != 0).then_some(text.len())
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
    let width = prog.members.len() + 1;
    let mut state = cache.start(prog, from);
    let mut last = None;
    for (at, &b) in text.iter().enumerate().skip(from) {
        if state & MATCH != 0 {
            last = Some(at);
        }
        if state == DEAD {
            return last;
        }
        *work = work.saturating_sub(1);
        let class = usize::from(prog.classes[usize::from(b)]);
        state = match cache.table[(state & !MATCH) as usize * width + class] {
            UNKNOWN => cache.step(prog, state, class),
            known => known,
        };
    }
    if state == DEAD {
        return last;
    }
    if state & MATCH != 0 {
        return Some(text.len());
    }
    let end = match cache.table[state as usize * width + width - 1] {
        UNKNOWN => cache.finish(prog, state),
        known => known,
    };
    if end & MATCH != 0 {
        Some(text.len())
    } else {
        last
    }
}
