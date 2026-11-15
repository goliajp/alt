//! Matching by stepping every live path of the automaton at once, each
//! carrying the positions of its groups. One pass over the text, with
//! work per byte bounded by the size of the pattern.

use crate::class::ByteSet;
use crate::program::{Inst, Program};
use crate::syntax::Look;

/// A position no group has.
pub const UNSET: usize = usize::MAX;

/// The paths alive at one position, in order of preference.
#[derive(Default)]
struct Threads {
    /// Where each instruction sits in `order`, when it is in the set.
    place: Vec<u32>,
    order: Vec<u32>,
    /// The capture slots of each path, in the order of `order`.
    slots: Vec<usize>,
}

impl Threads {
    fn reset(&mut self, insts: usize, width: usize) {
        self.place.resize(insts, 0);
        self.order.clear();
        self.slots.resize(insts * width, UNSET);
    }

    fn contains(&self, pc: u32) -> bool {
        let at = self.place[pc as usize] as usize;
        self.order.get(at) == Some(&pc)
    }

    fn insert(&mut self, pc: u32) -> usize {
        self.place[pc as usize] = self.order.len() as u32;
        self.order.push(pc);
        self.order.len() - 1
    }
}

enum Frame {
    Explore(u32),
    Restore(usize, usize),
}

/// Storage a search reuses.
#[derive(Default)]
pub struct Cache {
    now: Threads,
    next: Threads,
    stack: Vec<Frame>,
    scratch: Vec<usize>,
}

fn holds(look: Look, text: &[u8], at: usize) -> bool {
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

/// Adds the path at `pc`, and every path reachable from it without
/// consuming a byte, to `list`.
fn follow(
    prog: &Program,
    list: &mut Threads,
    stack: &mut Vec<Frame>,
    slots: &mut [usize],
    pc: u32,
    text: &[u8],
    at: usize,
) {
    let width = slots.len();
    stack.push(Frame::Explore(pc));
    while let Some(frame) = stack.pop() {
        let pc = match frame {
            Frame::Restore(slot, old) => {
                slots[slot] = old;
                continue;
            }
            Frame::Explore(pc) if list.contains(pc) => continue,
            Frame::Explore(pc) => pc,
        };
        let index = list.insert(pc);
        match prog.insts[pc as usize] {
            Inst::Split(first, second) => {
                stack.push(Frame::Explore(second));
                stack.push(Frame::Explore(first));
            }
            Inst::Save(slot, next) => {
                let slot = slot as usize;
                if slot < width {
                    stack.push(Frame::Restore(slot, slots[slot]));
                    slots[slot] = at;
                }
                stack.push(Frame::Explore(next));
            }
            Inst::Look(look, next) => {
                if holds(look, text, at) {
                    stack.push(Frame::Explore(next));
                }
            }
            Inst::Set(..) | Inst::Byte(..) | Inst::Match => {
                list.slots[index * width..][..width].copy_from_slice(slots);
            }
        }
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
    let width = found.len();
    let Cache {
        now,
        next,
        stack,
        scratch,
    } = cache;
    now.reset(prog.insts.len(), width);
    next.reset(prog.insts.len(), width);
    scratch.clear();
    scratch.resize(width, UNSET);
    let mut matched = false;
    let from = only.unwrap_or(0);
    for at in from..=text.len() {
        if !matched && (at == from || (only.is_none() && !prog.anchored)) {
            scratch.fill(UNSET);
            follow(prog, now, stack, scratch, prog.start, text, at);
        }
        if now.order.is_empty() {
            break;
        }
        for index in 0..now.order.len() {
            let slots = &now.slots[index * width..][..width];
            // a path that began behind the match found cannot replace it
            if matched && slots[0] > found[0] {
                continue;
            }
            let target = match prog.insts[now.order[index] as usize] {
                Inst::Match => {
                    let better = !matched || slots[0] < found[0] || at > found[1];
                    if better {
                        found.copy_from_slice(slots);
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
                _ => continue,
            };
            scratch.copy_from_slice(slots);
            follow(prog, next, stack, scratch, target, text, at + 1);
        }
        std::mem::swap(now, next);
        next.order.clear();
    }
    matched
}
