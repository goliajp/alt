//! Matching by stepping every live path of the automaton at once, each
//! carrying the positions of its groups. One pass over the text, with
//! work per byte bounded by the size of the pattern.

mod follow;

use crate::class::ByteSet;
use crate::program::{Inst, Program};
use crate::syntax::Look;
use follow::{Frame, Walk};

/// A position no group has.
pub const UNSET: usize = usize::MAX;

/// The most capture slots the paths that wait at one position of the
/// text carry between them: 8 MiB, and as much again for the paths at
/// the next position.
pub const MAX_LIST: usize = 1 << 20;

/// The paths that wait at one position, in order of preference.
#[derive(Default)]
struct Threads {
    /// the instruction each waits at
    order: Vec<u32>,
    /// the capture slots of each, one after another
    slots: Vec<usize>,
    /// A path came to the match behind a condition; its slots are `last`.
    late: bool,
    last: Vec<usize>,
}

impl Threads {
    fn clear(&mut self) {
        self.order.clear();
        self.slots.clear();
        self.late = false;
    }
}

/// Storage a search reuses.
#[derive(Default)]
pub struct Cache {
    now: Threads,
    next: Threads,
    /// For each instruction the turn, one to a position of the text, in
    /// which a path last came to it; behind a condition, in `passed`.
    seen: Vec<u32>,
    passed: Vec<u32>,
    /// For an instruction on the path being followed, how many were on
    /// it when the path last came there.
    depth: Vec<u32>,
    turn: u32,
    /// how often in this turn a path may still come back to an
    /// instruction it is on
    rounds: u32,
    stack: Vec<Frame>,
    scratch: Vec<usize>,
    /// The last search was given up: its paths came to carry more than
    /// [`MAX_LIST`] slots.
    pub exhausted: bool,
}

impl Cache {
    /// Begins the next turn, with the marks of the paths in `now` kept.
    fn turn(&mut self) {
        if self.turn == u32::MAX {
            self.seen.fill(0);
            self.passed.fill(0);
            self.turn = 0;
            for &pc in &self.now.order {
                self.seen[pc as usize] = 1;
            }
        }
        self.turn += 1;
        self.rounds = follow::ROUNDS;
    }

    fn reset(&mut self, prog: &Program, width: usize) {
        let insts = prog.insts.len();
        self.seen.resize(insts, 0);
        self.depth.resize(if prog.cyclic { insts } else { 0 }, 0);
        self.passed
            .resize(if prog.mixed_ends { insts } else { 0 }, 0);
        self.now.clear();
        self.next.clear();
        self.scratch.clear();
        self.scratch.resize(width, UNSET);
        self.stack.clear();
        self.exhausted = false;
        self.turn();
    }
}

/// Whether the condition holds at `at`; `utf8` for a text matched
/// character by character.
pub fn holds(look: Look, text: &[u8], at: usize, utf8: bool) -> bool {
    let edge = !matches!(
        look,
        Look::TextStart | Look::TextEnd | Look::LineStart | Look::LineEnd
    );
    if utf8 && edge {
        return crate::chars::holds(look, text, at);
    }
    let word = ByteSet::word();
    let before = at > 0 && word.contains(text[at - 1]);
    let after = at < text.len() && word.contains(text[at]);
    match look {
        Look::TextStart => at == 0,
        Look::TextEnd => at == text.len(),
        Look::LineStart => at == 0 || text[at - 1] == b'\n',
        Look::LineEnd => at == text.len() || text[at] == b'\n',
        Look::WordBoundary => before != after,
        Look::NotWordBoundary => before == after,
        Look::WordStart => !before && after,
        Look::WordEnd => before && !after,
    }
}

/// Finds the match that starts earliest and, of those, ends latest;
/// fills `found` with its capture slots. `found` holds at least the two
/// slots of the whole match. With `only`, a match begins at that
/// position or nowhere.
pub fn search(
    prog: &Program,
    cache: &mut Cache,
    text: &[u8],
    only: Option<usize>,
    found: &mut [usize],
) -> bool {
    let captures = found.len();
    // the whole match alone is found without the snapshot
    let width = captures * (1 + usize::from(prog.snapshots && captures > 2));
    cache.reset(prog, width);
    let mut matched = false;
    let from = only.unwrap_or(0);
    for at in from..=text.len() {
        let begins = at == from || (only.is_none() && !prog.anchored);
        if !matched && begins && !(prog.apart && crate::chars::inside(text, at)) {
            cache.scratch.fill(UNSET);
            let (mut walk, slots) = Walk::over(prog, cache, text, captures, false);
            walk.follow(slots, prog.start, at);
        }
        if cache.now.order.is_empty() && !cache.now.late {
            // nothing lives, and nothing begins later
            if matched || only.is_some() || prog.anchored {
                break;
            }
            cache.turn();
            continue;
        }
        cache.turn();
        for index in 0..cache.now.order.len() {
            let pc = cache.now.order[index] as usize;
            let slots = &cache.now.slots[index * width..][..width];
            // a path that began behind the match found cannot replace it
            if matched && slots[0] > found[0] {
                continue;
            }
            let target = match prog.insts[pc] {
                Inst::Match => {
                    if !matched || slots[0] < found[0] || at > found[1] {
                        found.copy_from_slice(&slots[..captures]);
                        found[1] = at;
                        matched = true;
                    }
                    continue;
                }
                Inst::Byte(b, target) if text.get(at) == Some(&b) => target,
                Inst::Set(set, target)
                    if text
                        .get(at)
                        .is_some_and(|&b| prog.sets[set as usize].contains(b)) =>
                {
                    target
                }
                Inst::Switch(copied, place) => {
                    let taken = text.get(at).and_then(|&b| prog.taken(copied, place, b));
                    match taken {
                        Some(target) => target,
                        None => continue,
                    }
                }
                _ => continue,
            };
            cache.scratch.copy_from_slice(slots);
            let (mut walk, slots) = Walk::over(prog, cache, text, captures, true);
            walk.follow(slots, target, at + 1);
        }
        if cache.exhausted {
            return false;
        }
        // a path that came to the match behind a condition is kept when
        // none that began as early came without one
        let slots = &cache.now.last;
        if cache.now.late
            && (!matched || slots[0] < found[0] || (slots[0] == found[0] && at > found[1]))
        {
            found.copy_from_slice(&slots[..captures]);
            found[1] = at;
            matched = true;
        }
        std::mem::swap(&mut cache.now, &mut cache.next);
        cache.next.clear();
    }
    matched
}
