//! Matching a pattern with back-references, which no automaton can: the
//! paths from each start are tried one after another, in order of
//! preference, and the longest match of the earliest start that has one
//! is kept. The work is bounded by a count of steps; a search that would
//! take more reports no match.

use crate::pike::{UNSET, holds};
use crate::program::{Inst, Program};

/// The most steps one search takes.
pub const MAX_STEPS: usize = 1 << 22;

enum Frame {
    Step(u32, usize),
    Slot(usize, usize),
    /// where a loop was last entered on this path
    Loop(u32, usize),
}

fn same(a: &[u8], b: &[u8], fold: bool) -> bool {
    match fold {
        true => a.eq_ignore_ascii_case(b),
        false => a == b,
    }
}

/// Fills `found` with the first capture slots of the earliest match and,
/// of those, the longest. With `only`, a match begins there or nowhere.
pub fn search(prog: &Program, text: &[u8], only: Option<usize>, found: &mut [usize]) -> bool {
    let mut slots = vec![UNSET; 2 * (prog.groups as usize + 1)];
    let mut best = slots.clone();
    let mut entered = vec![UNSET; prog.insts.len()];
    let mut stack = Vec::new();
    let mut steps = MAX_STEPS;
    let first = only.unwrap_or(0);
    let last = match only.is_some() || prog.anchored {
        true => first,
        false => text.len(),
    };
    for from in first..=last {
        let mut matched = false;
        slots.fill(UNSET);
        entered.fill(UNSET);
        stack.push(Frame::Step(prog.start, from));
        'paths: while let Some(frame) = stack.pop() {
            let (mut pc, mut at) = match frame {
                Frame::Slot(slot, old) => {
                    slots[slot] = old;
                    continue;
                }
                Frame::Loop(pc, old) => {
                    entered[pc as usize] = old;
                    continue;
                }
                Frame::Step(pc, at) => (pc, at),
            };
            loop {
                if steps == 0 {
                    return false;
                }
                steps -= 1;
                match prog.insts[pc as usize] {
                    Inst::Byte(b, next) if text.get(at) == Some(&b) => (pc, at) = (next, at + 1),
                    Inst::Set(set, next)
                        if text
                            .get(at)
                            .is_some_and(|&b| prog.sets[set as usize].contains(b)) =>
                    {
                        (pc, at) = (next, at + 1)
                    }
                    Inst::Split(first, second) => {
                        // behind a turn of a loop that took nothing, the
                        // only way on is out of the loop
                        let before = std::mem::replace(&mut entered[pc as usize], at);
                        if before == at {
                            pc = second;
                            continue;
                        }
                        stack.push(Frame::Loop(pc, before));
                        stack.push(Frame::Step(second, at));
                        pc = first;
                    }
                    // what a group last matched is what its reference
                    // matches again, also where the group took nothing
                    Inst::Save(slot, next) | Inst::SaveOpt(slot, next) => {
                        let slot = slot as usize;
                        stack.push(Frame::Slot(slot, slots[slot]));
                        slots[slot] = at;
                        pc = next;
                    }
                    Inst::Look(look, next) if holds(look, text, at) => pc = next,
                    Inst::BackRef(group, fold, next) => {
                        let (from, to) = (slots[2 * group as usize], slots[2 * group as usize + 1]);
                        if from == UNSET || to == UNSET {
                            break;
                        }
                        let again = text.get(at..at + (to - from));
                        if !again.is_some_and(|again| same(again, &text[from..to], fold)) {
                            break;
                        }
                        (pc, at) = (next, at + (to - from));
                    }
                    Inst::Match => {
                        if !matched || at > best[1] {
                            best.copy_from_slice(&slots);
                            best[1] = at;
                            matched = true;
                        }
                        if at == text.len() {
                            break 'paths;
                        }
                        break;
                    }
                    _ => break,
                }
            }
        }
        stack.clear();
        if matched {
            found.copy_from_slice(&best[..found.len()]);
            return true;
        }
    }
    false
}
