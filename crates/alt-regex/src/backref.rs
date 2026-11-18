//! Matching a pattern with back-references, which no automaton can: the
//! paths from each start are tried one after another, in order of
//! preference, and the longest match of the earliest start that has one
//! is kept. The work is bounded by a count of steps, and a path by a
//! count of the choices it leaves open; a search that would take more
//! reports no match.
//!
//! Asked for fewer groups than the pattern refers to, the C library
//! finds the match as ever and then walks one path through it with the
//! registers it was given: a walk that fails is no match at all, and
//! [`crate::walk`] is that walk.

use crate::pike::{UNSET, holds};
use crate::program::{Inst, NO_JOIN, Program};

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

/// The places a search has stood at: an instruction where paths meet, a
/// position, and what the groups referred to held. From a place it
/// stood at before, nothing new is found. It is a table of what was
/// seen last, 2 MiB of it, made once a search has taken some steps.
struct Stood {
    table: Vec<[u64; 2]>,
    /// the capture slots of the groups referred to
    referred: Vec<usize>,
}

impl Stood {
    const PLACES: usize = 1 << 17;
    /// A search of fewer steps keeps no table.
    const AFTER: usize = 1 << 10;

    fn new(prog: &Program) -> Self {
        let mut referred = Vec::new();
        for inst in &prog.insts {
            if let Inst::BackRef(group, ..) = *inst {
                referred.extend([2 * group as usize, 2 * group as usize + 1]);
            }
        }
        referred.sort_unstable();
        referred.dedup();
        let table = Vec::new();
        Stood { table, referred }
    }

    /// Whether the search stood here before; it has now. A loop come
    /// back to with nothing taken is no place to ask of: it is left.
    fn before(&mut self, place: [usize; 3], slots: &[usize]) -> bool {
        let mut key = [0x9e37_79b9_7f4a_7c15u64, 0xc2b2_ae3d_27d4_eb4f];
        let held = self.referred.iter().map(|&slot| slots[slot]);
        for value in place.into_iter().chain(held) {
            key[0] = (key[0] ^ value as u64).wrapping_mul(0xff51_afd7_ed55_8ccd);
            key[0] ^= key[0] >> 32;
            key[1] = (key[1].rotate_left(23) ^ value as u64).wrapping_mul(0xc4ce_b9fe_1a85_ec53);
        }
        key[1] |= 1;
        if self.table.is_empty() {
            self.table = vec![[0; 2]; Self::PLACES];
        }
        let at = key[0] as usize % Self::PLACES;
        std::mem::replace(&mut self.table[at], key) == key
    }
}

fn same(a: &[u8], b: &[u8], fold: bool) -> bool {
    match fold {
        true => a.eq_ignore_ascii_case(b),
        false => a == b,
    }
}

/// How many bytes at `at` are what a group last matched from `from` to
/// `to`, if it took part and they are; with `fold`, letters in either
/// case.
pub fn again(
    prog: &Program,
    text: &[u8],
    (from, to): (usize, usize),
    fold: bool,
    at: usize,
) -> Option<usize> {
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
    let width = found.len();
    let mut steps = MAX_STEPS;
    if width == 2 || 2 * prog.referred as usize + 2 <= width {
        return paths(prog, text, only, found, &mut steps);
    }
    let mut whole = [UNSET; 2];
    if !paths(prog, text, only, &mut whole, &mut steps)? {
        return Some(false);
    }
    steps = MAX_STEPS;
    crate::walk::walk(prog, text, (whole[0], whole[1]), found, &mut steps)
}

fn paths(
    prog: &Program,
    text: &[u8],
    only: Option<usize>,
    found: &mut [usize],
    steps: &mut usize,
) -> Option<bool> {
    let mut slots = vec![UNSET; 2 * (prog.groups as usize + 1)];
    let mut best = slots.clone();
    let mut entered = vec![UNSET; prog.insts.len()];
    let mut stack = Vec::new();
    let mut stood = Stood::new(prog);
    let first = only.unwrap_or(0);
    let fixed = only.is_some() || prog.anchored;
    for from in first..=if fixed { first } else { text.len() } {
        if prog.apart && crate::chars::inside(text, from) {
            continue;
        }
        // whether a match was found, and whether behind a condition
        let mut matched = None;
        slots.fill(UNSET);
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
                if *steps == 0 || stack.len() + 2 > MAX_FRAMES {
                    return None;
                }
                *steps -= 1;
                let place = [pc as usize | usize::from(behind) << 32, at, from];
                let met = prog.joins[pc as usize] != NO_JOIN && entered[pc as usize] != at;
                if met && *steps < MAX_STEPS - Stood::AFTER && stood.before(place, &slots) {
                    break;
                }
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
                        // the way out is taken with the loop as it was
                        // found: a loop around it may enter it again
                        let out = Frame::Step(second | if behind { BEHIND } else { 0 }, at);
                        stack.extend([out, Frame::Loop(pc, before)]);
                        pc = first;
                    }
                    // what a group last matched is what its reference
                    // matches again, also where the group took nothing
                    Inst::Save(slot, next) | Inst::SaveOpt(slot, next) => {
                        pc = next;
                        stack.push(Frame::Slot(slot, slots[slot as usize]));
                        slots[slot as usize] = at;
                    }
                    Inst::Look(look, next) if holds(look, text, at, prog.utf8) => {
                        (pc, behind) = (next, true)
                    }
                    Inst::BackRef(group, fold, next) => {
                        let lies = (slots[2 * group as usize], slots[2 * group as usize + 1]);
                        let Some(taken) = again(prog, text, lies, fold, at) else {
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
                        let last = at == text.len() && !(behind && prog.mixed_ends);
                        if last {
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
