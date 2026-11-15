//! Matching by stepping every live path of the automaton at once, each
//! carrying the positions of its groups. One pass over the text, with
//! work per byte bounded by the size of the pattern.

use crate::class::ByteSet;
use crate::program::{Inst, NO_JOIN, Program, close};
use crate::syntax::Look;

/// A position no group has.
pub const UNSET: usize = usize::MAX;

/// The paths alive at one position, in order of preference.
#[derive(Default)]
struct Threads {
    /// Where each instruction sits in `order`, when it is in the set.
    place: Vec<u32>,
    order: Vec<u32>,
    /// The capture slots of each path that waits at an instruction, by
    /// the row of the instruction.
    slots: Vec<usize>,
    /// The instructions reached behind a condition, with nothing
    /// consumed since, that do not wait for a byte: a path without the
    /// condition may still pass them.
    behind: Vec<u32>,
    reached: Vec<u32>,
    /// A path came to the match that way; its slots are the last row.
    late: bool,
}

impl Threads {
    fn reset(&mut self, insts: usize, rows: usize, width: usize) {
        self.place.resize(insts, 0);
        self.behind.resize(insts, 0);
        self.slots.resize((rows + 1) * width, UNSET);
        self.clear();
    }

    fn clear(&mut self) {
        self.order.clear();
        self.reached.clear();
        self.late = false;
    }

    fn passed(&self, pc: u32) -> bool {
        let at = self.behind[pc as usize] as usize;
        self.reached.get(at) == Some(&pc)
    }

    fn pass(&mut self, pc: u32) {
        self.behind[pc as usize] = self.reached.len() as u32;
        self.reached.push(pc);
    }

    fn contains(&self, pc: u32) -> bool {
        let at = self.place[pc as usize] as usize;
        self.order.get(at) == Some(&pc)
    }

    fn insert(&mut self, pc: u32) {
        self.place[pc as usize] = self.order.len() as u32;
        self.order.push(pc);
    }
}

enum Frame {
    /// an instruction, and whether a condition was met on the way
    Explore(u32, bool),
    Restore(usize, usize),
    /// put every slot back from the top of the copies kept
    Whole,
}

/// Storage a search reuses.
#[derive(Default)]
pub struct Cache {
    now: Threads,
    next: Threads,
    stack: Vec<Frame>,
    scratch: Vec<usize>,
    /// whole sets of slots, to put back
    kept: Vec<usize>,
}

pub fn holds(look: Look, text: &[u8], at: usize) -> bool {
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
#[allow(clippy::too_many_arguments)]
fn follow(
    prog: &Program,
    list: &mut Threads,
    stack: &mut Vec<Frame>,
    kept: &mut Vec<usize>,
    slots: &mut [usize],
    captures: usize,
    pc: u32,
    text: &[u8],
    at: usize,
) {
    let width = slots.len();
    stack.push(Frame::Explore(pc, false));
    while let Some(frame) = stack.pop() {
        let (pc, behind) = match frame {
            Frame::Restore(slot, old) => {
                slots[slot] = old;
                continue;
            }
            Frame::Whole => {
                slots.copy_from_slice(&kept[kept.len() - width..]);
                kept.truncate(kept.len() - width);
                continue;
            }
            Frame::Explore(pc, _) if list.contains(pc) => continue,
            Frame::Explore(pc, true) if list.passed(pc) => continue,
            Frame::Explore(pc, behind) => (pc, behind),
        };
        let inst = prog.insts[pc as usize];
        match (behind, inst) {
            (true, Inst::Match) => {
                list.pass(pc);
                list.late = true;
                let row = prog.row_count as usize;
                list.slots[row * width..][..width].copy_from_slice(slots);
                continue;
            }
            (false, _) | (true, Inst::Set(..) | Inst::Byte(..)) => list.insert(pc),
            (true, _) => list.pass(pc),
        }
        match inst {
            Inst::Split(first, second) => {
                stack.push(Frame::Explore(second, behind));
                stack.push(Frame::Explore(first, behind));
            }
            Inst::Save(slot, next) | Inst::SaveOpt(slot, next) => {
                let slot = slot as usize;
                if slot < captures && prog.snapshots {
                    kept.extend_from_slice(slots);
                    stack.push(Frame::Whole);
                    let opt = matches!(prog.insts[pc as usize], Inst::SaveOpt(..));
                    close(prog, slots, captures, slot, at, opt);
                } else if slot < captures {
                    stack.push(Frame::Restore(slot, slots[slot]));
                    slots[slot] = at;
                }
                stack.push(Frame::Explore(next, behind));
            }
            Inst::Look(look, next) => {
                if holds(look, text, at) {
                    stack.push(Frame::Explore(next, prog.mixed_ends));
                }
            }
            // a pattern with one is not searched this way
            Inst::BackRef(..) => {}
            Inst::Set(..) | Inst::Byte(..) | Inst::Match => {
                let row = prog.rows[pc as usize] as usize;
                list.slots[row * width..][..width].copy_from_slice(slots);
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
    let captures = found.len();
    let width = if prog.snapshots {
        2 * captures
    } else {
        captures
    };
    let Cache {
        now,
        next,
        stack,
        scratch,
        kept,
    } = cache;
    kept.clear();
    now.reset(prog.insts.len(), prog.row_count as usize, width);
    next.reset(prog.insts.len(), prog.row_count as usize, width);
    scratch.clear();
    scratch.resize(width, UNSET);
    let mut matched = false;
    let from = only.unwrap_or(0);
    for at in from..=text.len() {
        if !matched && (at == from || (only.is_none() && !prog.anchored)) {
            scratch.fill(UNSET);
            follow(
                prog, now, stack, kept, scratch, captures, prog.start, text, at,
            );
        }
        if now.order.is_empty() {
            break;
        }
        for index in 0..now.order.len() {
            let pc = now.order[index] as usize;
            let row = prog.rows[pc] as usize;
            if row == NO_JOIN as usize {
                continue;
            }
            let slots = &now.slots[row * width..][..width];
            // a path that began behind the match found cannot replace it
            if matched && slots[0] > found[0] {
                continue;
            }
            let target = match prog.insts[pc] {
                Inst::Match => {
                    let better = !matched || slots[0] < found[0] || at > found[1];
                    if better {
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
                _ => continue,
            };
            scratch.copy_from_slice(slots);
            follow(
                prog,
                next,
                stack,
                kept,
                scratch,
                captures,
                target,
                text,
                at + 1,
            );
        }
        // a path that came to the match behind a condition is kept when
        // none that began as early came without one
        if now.late {
            let slots = &now.slots[prog.row_count as usize * width..][..width];
            let better = match matched {
                false => true,
                true => slots[0] < found[0] || (slots[0] == found[0] && at > found[1]),
            };
            if better {
                found.copy_from_slice(&slots[..captures]);
                found[1] = at;
                matched = true;
            }
        }
        std::mem::swap(now, next);
        next.clear();
    }
    matched
}
