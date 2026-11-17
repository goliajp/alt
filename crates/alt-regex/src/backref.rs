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
//! [`Walk`] is that walk.

use crate::pike::{UNSET, holds};
use crate::program::{Inst, NO_JOIN, Program};
use crate::registers::close;

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

    /// A walk turns on the slots at `kept` as well.
    fn new(prog: &Program, kept: std::ops::Range<usize>) -> Self {
        let mut referred = Vec::new();
        for inst in &prog.insts {
            if let Inst::BackRef(group, ..) = *inst {
                referred.extend([2 * group as usize, 2 * group as usize + 1]);
            }
        }
        referred.sort_unstable();
        referred.dedup();
        referred.extend(kept);
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

/// A path through a match that is known. The walk of the C library is
/// one: it keeps the `width` capture slots that library was given and,
/// behind them, what they were when a group last closed around
/// something; a group beyond them it does not keep. With no slots to
/// keep, the path is only asked whether it is one.
#[derive(Clone, Copy)]
struct Walk {
    /// the instruction it begins at
    begins: u32,
    end: usize,
    width: usize,
}

impl Walk {
    /// Whether a group that is kept has begun and not ended.
    fn open(self, kept: &[usize]) -> bool {
        let mut groups = kept[2..self.width].as_chunks::<2>().0.iter();
        groups.any(|group| group[0] != UNSET && group[1] == UNSET)
    }

    /// Records the end `slot` of a group at `at` in what is `kept`, as
    /// the C library keeps its registers.
    fn keep(
        self,
        prog: &Program,
        kept: &mut [usize],
        (slot, at, opt): (usize, usize, bool),
        mut undo: impl FnMut(usize, usize),
    ) {
        // the ends of the match are known, and a group merged with
        // another is not kept
        if slot < 2 || slot >= self.width || prog.merged[slot / 2] as usize != slot / 2 {
            return;
        }
        close(kept, self.width, slot, at, opt, &mut undo);
        // a group that begins has no end yet
        if slot.is_multiple_of(2) && kept[slot + 1] != UNSET {
            undo(slot + 1, kept[slot + 1]);
            kept[slot + 1] = UNSET;
        }
    }

    /// Whether a path of the match goes on from the instruction and
    /// the position `place`, the groups being `groups`: the C library
    /// walks only where one does.
    fn goes_on(
        self,
        prog: &Program,
        text: &[u8],
        (pc, at): (u32, usize),
        groups: &[usize],
        steps: &mut usize,
    ) -> Option<bool> {
        let on = Walk {
            begins: pc,
            width: 0,
            ..self
        };
        // what such a search sets up counts as steps taken
        *steps = steps.checked_sub(prog.insts.len())?;
        paths(prog, text, Some(at), &mut groups.to_vec(), Some(on), steps)
    }
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
        return paths(prog, text, only, found, None, &mut steps);
    }
    let mut whole = [UNSET; 2];
    if !paths(prog, text, only, &mut whole, None, &mut steps)? {
        return Some(false);
    }
    let walk = Walk {
        begins: prog.start,
        end: whole[1],
        width,
    };
    steps = MAX_STEPS;
    paths(prog, text, Some(whole[0]), found, Some(walk), &mut steps)
}

/// The search, or with `walk` that path from `only`; one that keeps no
/// slots goes on from the groups in `found`.
fn paths(
    prog: &Program,
    text: &[u8],
    only: Option<usize>,
    found: &mut [usize],
    walk: Option<Walk>,
    steps: &mut usize,
) -> Option<bool> {
    // the groups of the path, and behind them what a walk keeps
    let all = 2 * (prog.groups as usize + 1);
    let width = walk.map_or(0, |walk| walk.width);
    let mut slots = vec![UNSET; all + 2 * width];
    let mut best = slots.clone();
    let mut entered = vec![UNSET; prog.insts.len()];
    let mut stack = Vec::new();
    // how many other ways the path has left behind it
    let mut ways;
    let mut stood = Stood::new(prog, all..all + 2 * width);
    // a group closed on a walk may put every slot back
    let room = 2 + width;
    let first = only.unwrap_or(0);
    let fixed = only.is_some() || prog.anchored;
    for from in first..=if fixed { first } else { text.len() } {
        if prog.apart && crate::chars::inside(text, from) {
            continue;
        }
        // whether a match was found, and whether behind a condition
        let mut matched = None;
        slots.fill(UNSET);
        match walk {
            Some(_) if width == 0 => slots.copy_from_slice(found),
            Some(walk) => {
                slots[all..all + 2].copy_from_slice(&[from, walk.end]);
                slots[all + width..all + width + 2].copy_from_slice(&[from, walk.end]);
            }
            None => {}
        }
        let begins = walk.map_or(prog.start, |walk| walk.begins);
        stack.push(Frame::Step(begins, from));
        ways = 1usize;
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
            ways -= 1;
            loop {
                if *steps == 0 || stack.len() + room > MAX_FRAMES {
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
                            let Some(walk) = walk.filter(|_| width > 0) else {
                                pc = second;
                                continue;
                            };
                            // come back to where it stood, the walk of
                            // the C library ends, or with a group left
                            // open takes another way if it has one
                            let groups = &slots[..all];
                            let open = walk.open(&slots[all..]) && ways > 0;
                            if open || !walk.goes_on(prog, text, (pc, at), groups, steps)? {
                                break;
                            }
                            best.copy_from_slice(&slots);
                            matched = Some(behind);
                            break 'paths;
                        }
                        // the way out is taken with the loop as it was
                        // found: a loop around it may enter it again.
                        // The walk of the C library knows it came by
                        let out = Frame::Step(second | if behind { BEHIND } else { 0 }, at);
                        let mark = Frame::Loop(pc, before);
                        stack.extend(if width > 0 { [mark, out] } else { [out, mark] });
                        ways += 1;
                        pc = first;
                    }
                    // what a group last matched is what its reference
                    // matches again, also where the group took nothing
                    Inst::Save(slot, next) | Inst::SaveOpt(slot, next) => {
                        let slot = slot as usize;
                        let opt = matches!(prog.insts[pc as usize], Inst::SaveOpt(..));
                        pc = next;
                        stack.push(Frame::Slot(slot as u32, slots[slot]));
                        slots[slot] = at;
                        if let Some(walk) = walk.filter(|_| width > 0) {
                            walk.keep(prog, &mut slots[all..], (slot, at, opt), |slot, old| {
                                stack.push(Frame::Slot((all + slot) as u32, old))
                            });
                        }
                    }
                    Inst::Look(look, next) if holds(look, text, at, prog.utf8) => {
                        (pc, behind) = (next, true)
                    }
                    // of a group it does not keep the walk knows
                    // nothing: where it can turn back it does, and
                    // elsewhere takes the group to be empty
                    Inst::BackRef(group, _, next)
                        if width > 0 && 2 * prog.merged[group as usize] as usize >= width =>
                    {
                        if prog.plural {
                            break;
                        }
                        pc = next;
                    }
                    Inst::BackRef(group, fold, next) => {
                        let (held, group) = match width {
                            0 => (0, group),
                            _ => (all, prog.merged[group as usize]),
                        };
                        let Some(taken) = again(prog, text, &slots[held..], group, fold, at) else {
                            break;
                        };
                        behind &= taken == 0;
                        (pc, at) = (next, at + taken);
                    }
                    Inst::Match if walk.is_some_and(|walk| walk.end != at) => break,
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
                        if last || walk.is_some() {
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
            let held = if width > 0 { all } else { 0 };
            found.copy_from_slice(&best[held..held + found.len()]);
            for (group, kept) in prog.merged.iter().enumerate().take(width / 2) {
                let kept = 2 * *kept as usize;
                found.copy_within(kept..kept + 2, 2 * group);
            }
            return Some(true);
        }
    }
    Some(false)
}
