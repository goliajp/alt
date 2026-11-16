//! Matching a pattern with back-references, which no automaton can: the
//! paths from each start are tried one after another, in order of
//! preference, and the longest match of the earliest start that has one
//! is kept. The work is bounded by a count of steps, and a path by a
//! count of the choices it leaves open; a search that would take more
//! reports no match.

use crate::pike::{UNSET, holds};
use crate::program::{Inst, Program};

/// The most steps one search takes.
pub const MAX_STEPS: usize = 1 << 22;
/// The most choices and slots written a path may leave behind it:
/// 16 MiB of them.
const MAX_FRAMES: usize = 1 << 20;
/// Set on the instruction of a step taken behind a condition, with no
/// byte consumed since.
const BEHIND: u32 = 1 << 31;

enum Frame {
    /// an instruction, with [`BEHIND`] or without, and a position
    Step(u32, usize),
    Slot(u32, usize),
    /// where a loop was last entered on this path
    Loop(u32, usize),
}

fn same(a: &[u8], b: &[u8], fold: bool) -> bool {
    match fold {
        true => a.eq_ignore_ascii_case(b),
        false => a == b,
    }
}

/// How many bytes at `at` are what `group` last matched, if it took part
/// and they are; with `fold`, letters in either case.
fn again(
    prog: &Program,
    text: &[u8],
    slots: &[usize],
    group: u32,
    fold: bool,
    at: usize,
) -> Option<usize> {
    let (from, to) = (slots[2 * group as usize], slots[2 * group as usize + 1]);
    if from == UNSET || to == UNSET {
        return None;
    }
    if fold && prog.utf8 {
        return crate::chars::again(&text[at..], &text[from..to]);
    }
    let taken = text.get(at..at + (to - from))?;
    same(taken, &text[from..to], fold).then_some(to - from)
}

/// Fills `found` with the first capture slots of the earliest match and,
/// of those, the longest. With `only`, a match begins there or nowhere.
/// `None` for a search given up at its bounds.
pub fn search(
    prog: &Program,
    text: &[u8],
    only: Option<usize>,
    found: &mut [usize],
) -> Option<bool> {
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
        if prog.apart && crate::chars::inside(text, from) {
            continue;
        }
        // whether a match was found, and whether behind a condition
        let mut matched = None;
        slots.fill(UNSET);
        entered.fill(UNSET);
        stack.push(Frame::Step(prog.start, from));
        'paths: while let Some(frame) = stack.pop() {
            let (mut pc, mut at, mut behind) = match frame {
                Frame::Slot(slot, old) => {
                    slots[slot as usize] = old;
                    continue;
                }
                Frame::Loop(pc, old) => {
                    entered[pc as usize] = old;
                    continue;
                }
                Frame::Step(pc, at) => (pc & !BEHIND, at, pc & BEHIND != 0),
            };
            loop {
                if steps == 0 || stack.len() + 2 > MAX_FRAMES {
                    return None;
                }
                steps -= 1;
                match prog.insts[pc as usize] {
                    Inst::Byte(b, next) if text.get(at) == Some(&b) => {
                        (pc, at, behind) = (next, at + 1, false)
                    }
                    Inst::Set(set, next)
                        if text
                            .get(at)
                            .is_some_and(|&b| prog.sets[set as usize].contains(b)) =>
                    {
                        (pc, at, behind) = (next, at + 1, false)
                    }
                    Inst::Switch(copied, place) => {
                        let taken = text.get(at).and_then(|&b| prog.taken(copied, place, b));
                        match taken {
                            Some(target) => (pc, at, behind) = (target, at + 1, false),
                            None => break,
                        }
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
                        stack.push(Frame::Step(second | if behind { BEHIND } else { 0 }, at));
                        pc = first;
                    }
                    // what a group last matched is what its reference
                    // matches again, also where the group took nothing
                    Inst::Save(slot, next) | Inst::SaveOpt(slot, next) => {
                        let slot = slot as usize;
                        stack.push(Frame::Slot(slot as u32, slots[slot]));
                        slots[slot] = at;
                        pc = next;
                    }
                    Inst::Look(look, next) if holds(look, text, at, prog.utf8) => {
                        (pc, behind) = (next, true)
                    }
                    Inst::BackRef(group, fold, next) => {
                        let Some(taken) = again(prog, text, &slots, group, fold, at) else {
                            break;
                        };
                        behind &= taken == 0;
                        (pc, at) = (next, at + taken);
                    }
                    Inst::Match => {
                        // of two paths as long, the C library keeps one
                        // that ends without a condition behind it
                        let better = match matched {
                            None => true,
                            Some(was) => at > best[1] || (at == best[1] && was && !behind),
                        };
                        if better {
                            best.copy_from_slice(&slots);
                            best[1] = at;
                            matched = Some(behind);
                        }
                        if at == text.len() && !(behind && prog.mixed_ends) {
                            break 'paths;
                        }
                        break;
                    }
                    _ => break,
                }
            }
        }
        stack.clear();
        if matched.is_some() {
            found.copy_from_slice(&best[..found.len()]);
            return Some(true);
        }
    }
    Some(false)
}
