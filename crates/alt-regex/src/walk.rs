//! The walk of the C library through a match it has found. Asked for
//! groups, it goes from the start of the match to its end along one
//! path and fills its registers on the way; with a back-reference in
//! the pattern, a walk that fails is no match at all. Asked for fewer
//! groups than the pattern refers to, the walk is what answers, and
//! this is that walk, step for step.
//!
//! It goes only where some path of the match stands ([`Alive`]). Where
//! two ways are open it takes the first and leaves the other behind
//! with the registers as they are; where it has passed the first
//! already with no byte taken since, it takes the second and leaves
//! nothing. A reference to a group it does not keep fails where the
//! pattern leaves a choice, and elsewhere takes the group to be empty.
//! A step that fails takes up the last way left behind, and with none
//! left there is no match.
//!
//! Come to the end of the match, or back to an instruction it has
//! passed with no byte taken since, the walk ends: as the match if no
//! group it keeps is begun and not ended, or if no way is left behind.
//! Otherwise it takes up the last way left behind, and does not record
//! the group which the instruction that way begins with begins or
//! ends. A group left begun and not ended is reported as one that
//! took no part.

use std::collections::{HashMap, HashSet};

use crate::backref::again;
use crate::pike::{UNSET, holds};
use crate::program::{Inst, Program};

/// The instructions and positions some path of the match stands at.
struct Alive {
    places: HashSet<(u32, usize)>,
}

/// A path as far as what lies ahead of it depends on: an instruction,
/// a position, and the number of what the groups referred to hold.
type Stand = (u32, usize, u32);

/// Every stand the paths from the start of a match come to.
struct Stands<'a> {
    prog: &'a Program,
    text: &'a [u8],
    end: usize,
    /// the capture slots of the groups referred to
    referred: Vec<usize>,
    held: Vec<Vec<usize>>,
    numbers: HashMap<Vec<usize>, u32>,
    stands: Vec<Stand>,
    known: HashMap<Stand, u32>,
    /// a stand and one it is come to from
    ways: Vec<(u32, u32)>,
}

impl Stands<'_> {
    /// More stands than this is a search given up.
    const MOST: usize = 1 << 18;

    fn number(&mut self, held: Vec<usize>) -> u32 {
        if let Some(&number) = self.numbers.get(&held) {
            return number;
        }
        self.held.push(held.clone());
        self.numbers.insert(held, self.held.len() as u32 - 1);
        self.held.len() as u32 - 1
    }

    fn come(&mut self, from: u32, stand: Stand) {
        let next = self.stands.len() as u32;
        let to = *self.known.entry(stand).or_insert(next);
        if to == next {
            self.stands.push(stand);
        }
        self.ways.push((to, from));
    }

    /// Where the group `group` lies in what `held` numbers.
    fn group(&self, held: u32, group: u32) -> (usize, usize) {
        let at = self.referred.binary_search(&(2 * group as usize));
        let at = at.expect("a group referred to");
        let held = &self.held[held as usize];
        (held[at], held[at + 1])
    }

    /// Goes on from the stand numbered `from` every way there is.
    fn step(&mut self, from: u32) {
        let (pc, at, held) = self.stands[from as usize];
        let (prog, text) = (self.prog, self.text);
        let byte = text[..self.end].get(at).copied();
        match prog.insts[pc as usize] {
            Inst::Byte(b, next) if byte == Some(b) => self.come(from, (next, at + 1, held)),
            Inst::Set(set, next) if byte.is_some_and(|b| prog.sets[set as usize].contains(b)) => {
                self.come(from, (next, at + 1, held))
            }
            Inst::Switch(copied, place) => {
                if let Some(next) = byte.and_then(|b| prog.taken(copied, place, b)) {
                    self.come(from, (next, at + 1, held));
                }
            }
            Inst::Split(first, second) => {
                self.come(from, (first, at, held));
                self.come(from, (second, at, held));
            }
            Inst::Save(slot, next) | Inst::SaveOpt(slot, next) => {
                let mut held = held;
                if let Ok(place) = self.referred.binary_search(&(slot as usize)) {
                    let mut now = self.held[held as usize].clone();
                    now[place] = at;
                    held = self.number(now);
                }
                self.come(from, (next, at, held));
            }
            Inst::Look(look, next) if holds(look, text, at, prog.utf8) => {
                self.come(from, (next, at, held))
            }
            Inst::BackRef(group, fold, next) => {
                let taken = again(prog, text, self.group(held, group), fold, at);
                if let Some(taken) = taken.filter(|taken| at + taken <= self.end) {
                    self.come(from, (next, at + taken, held));
                }
            }
            _ => {}
        }
    }
}

impl Alive {
    /// The places of the paths that match `text` from `from` to `end`.
    fn of(
        prog: &Program,
        text: &[u8],
        (from, end): (usize, usize),
        steps: &mut usize,
    ) -> Option<Self> {
        let mut referred = Vec::new();
        for inst in &prog.insts {
            if let Inst::BackRef(group, ..) = *inst {
                referred.extend([2 * group as usize, 2 * group as usize + 1]);
            }
        }
        referred.sort_unstable();
        referred.dedup();
        let mut all = Stands {
            prog,
            text,
            end,
            held: vec![vec![UNSET; referred.len()]],
            referred,
            numbers: HashMap::new(),
            stands: vec![(prog.start, from, 0)],
            known: HashMap::new(),
            ways: Vec::new(),
        };
        all.known.insert(all.stands[0], 0);
        let mut ends = Vec::new();
        let mut next = 0;
        while next < all.stands.len() {
            *steps = steps.checked_sub(1)?;
            if next >= Stands::MOST {
                return None;
            }
            let (pc, at, _) = all.stands[next];
            if at == end && prog.insts[pc as usize] == Inst::Match {
                ends.push(next as u32);
            }
            all.step(next as u32);
            next += 1;
        }
        // back from the ends of the match along the ways come
        all.ways.sort_unstable();
        let mut leads = vec![false; all.stands.len()];
        while let Some(stand) = ends.pop() {
            if std::mem::replace(&mut leads[stand as usize], true) {
                continue;
            }
            let first = all.ways.partition_point(|way| way.0 < stand);
            let from = all.ways[first..].iter().take_while(|way| way.0 == stand);
            ends.extend(from.map(|way| way.1));
        }
        let places = all.stands.iter().zip(leads).filter(|(_, leads)| *leads);
        Some(Alive {
            places: places.map(|(stand, _)| (stand.0, stand.1)).collect(),
        })
    }

    fn at(&self, pc: u32, at: usize) -> bool {
        self.places.contains(&(pc, at))
    }
}

/// A way left behind: where it goes on, the registers, and the
/// instructions passed with no byte taken, as they were.
struct Left {
    pc: u32,
    at: usize,
    regs: Vec<usize>,
    passed: Vec<u32>,
}

struct Walk<'a> {
    prog: &'a Program,
    text: &'a [u8],
    end: usize,
    alive: Alive,
    /// how many capture slots are kept
    width: usize,
    /// the slots, and what they were when a group last closed around
    /// something
    regs: Vec<usize>,
    /// the instructions passed since a byte was taken
    passed: Vec<u32>,
    left: Vec<Left>,
}

impl Walk<'_> {
    /// Records the group that the instruction `pc` begins or ends, as
    /// the C library keeps its registers.
    fn record(&mut self, pc: u32, at: usize) {
        let (slot, opt) = match self.prog.insts[pc as usize] {
            Inst::Save(slot, _) => (slot as usize, false),
            Inst::SaveOpt(slot, _) => (slot as usize, true),
            _ => return,
        };
        let width = self.width;
        if slot < 2 || slot >= width || self.prog.merged[slot / 2] as usize != slot / 2 {
            return;
        }
        let regs = &mut self.regs;
        if slot.is_multiple_of(2) {
            (regs[slot], regs[slot + 1]) = (at, UNSET);
        } else if regs[slot - 1] == UNSET || regs[slot - 1] < at {
            regs[slot] = at;
            regs.copy_within(..width, width);
        } else if opt && regs[width + slot - 1] != UNSET {
            regs.copy_within(width.., 0);
        } else {
            regs[slot] = at;
        }
    }

    /// Whether a group that is kept has begun and not ended.
    fn open(&self) -> bool {
        let mut groups = self.regs[..self.width].as_chunks::<2>().0.iter();
        groups.any(|group| group[0] != UNSET && group[1] == UNSET)
    }

    /// The instruction that `pc` is to that library: the two ends of
    /// the match and a group merged with another are no steps of its
    /// own there.
    fn node(&self, mut pc: u32) -> u32 {
        while let Inst::Save(slot, next) | Inst::SaveOpt(slot, next) = self.prog.insts[pc as usize]
        {
            if slot >= 2 && self.prog.merged[slot as usize / 2] == slot / 2 {
                break;
            }
            pc = next;
        }
        pc
    }

    /// Takes up the last way left behind.
    fn back(&mut self) -> Option<(u32, usize)> {
        let left = self.left.pop()?;
        (self.regs, self.passed) = (left.regs, left.passed);
        Some((left.pc, left.at))
    }

    /// The step from an instruction that takes no byte to `first` or
    /// `second`, whichever a path of the match stands at.
    fn turn(&mut self, plural: bool, at: usize, first: u32, second: u32) -> Option<(u32, usize)> {
        let (first, second) = (self.node(first), self.node(second));
        match (self.alive.at(first, at), self.alive.at(second, at)) {
            (false, false) => None,
            (true, false) => Some((first, at)),
            (false, true) => Some((second, at)),
            (true, true) if first == second || self.passed.contains(&first) => Some((second, at)),
            (true, true) => {
                if plural {
                    let (regs, passed) = (self.regs.clone(), self.passed.clone());
                    let pc = second;
                    self.left.push(Left {
                        pc,
                        at,
                        regs,
                        passed,
                    });
                }
                Some((first, at))
            }
        }
    }

    /// The step from `pc` at `at`, if the walk can take it.
    fn step(&mut self, pc: u32, at: usize) -> Option<(u32, usize)> {
        let (prog, text) = (self.prog, self.text);
        let plural = prog.plural;
        let (next, taken) = match prog.insts[pc as usize] {
            Inst::Split(first, second) => {
                self.passed.push(pc);
                return self.turn(plural, at, first, second);
            }
            Inst::Save(_, next) | Inst::SaveOpt(_, next) => (next, 0),
            Inst::Look(look, next) if holds(look, text, at, prog.utf8) => (next, 0),
            // of a group it does not keep the walk knows nothing: where
            // it can turn back it does, and elsewhere takes the group
            // to be empty
            Inst::BackRef(group, _, next)
                if 2 * prog.merged[group as usize] as usize >= self.width =>
            {
                if plural {
                    return None;
                }
                (next, 0)
            }
            Inst::BackRef(group, fold, next) => {
                let group = 2 * prog.merged[group as usize] as usize;
                let lies = (self.regs[group], self.regs[group + 1]);
                (next, again(prog, text, lies, fold, at)?)
            }
            Inst::Byte(b, next) if text.get(at) == Some(&b) => (next, 1),
            Inst::Set(set, next)
                if text
                    .get(at)
                    .is_some_and(|&b| prog.sets[set as usize].contains(b)) =>
            {
                (next, 1)
            }
            Inst::Switch(copied, place) => (prog.taken(copied, place, *text.get(at)?)?, 1),
            _ => return None,
        };
        let next = self.node(next);
        if taken == 0 {
            self.passed.push(pc);
            return self.alive.at(next, at).then_some((next, at));
        }
        let at = at + taken;
        if plural && (at > self.end || !self.alive.at(next, at)) {
            return None;
        }
        self.passed.clear();
        Some((next, at))
    }
}

/// Fills `found` with the capture slots the walk of the C library
/// leaves of the match from `from` to `end`; false where that walk
/// fails. `None` for a walk given up at its bounds.
pub fn walk(
    prog: &Program,
    text: &[u8],
    (from, end): (usize, usize),
    found: &mut [usize],
    steps: &mut usize,
) -> Option<bool> {
    let width = found.len();
    let mut regs = vec![UNSET; 2 * width];
    regs[..2].copy_from_slice(&[from, end]);
    regs.copy_within(..width, width);
    let mut walk = Walk {
        prog,
        text,
        end,
        alive: Alive::of(prog, text, (from, end), steps)?,
        width,
        regs,
        passed: Vec::new(),
        left: Vec::new(),
    };
    let (mut pc, mut at) = (walk.node(prog.start), from);
    loop {
        // what a way left behind holds counts as steps taken
        *steps = steps.checked_sub(1 + walk.passed.len() / 8)?;
        walk.record(pc, at);
        let ends = at == end && prog.insts[pc as usize] == Inst::Match;
        if ends || (prog.plural && walk.passed.contains(&pc)) {
            if !(prog.plural && walk.open()) {
                break;
            }
            match walk.back() {
                Some(left) => (pc, at) = left,
                None => break,
            }
        }
        (pc, at) = match walk.step(pc, at) {
            Some(on) => on,
            None => match walk.back() {
                Some(left) => left,
                None => return Some(false),
            },
        };
    }
    found.copy_from_slice(&walk.regs[..width]);
    for (group, kept) in prog.merged.iter().enumerate().take(width / 2) {
        let kept = 2 * *kept as usize;
        found.copy_within(kept..kept + 2, 2 * group);
    }
    Some(true)
}
