//! From where a path stands to every instruction it can wait at next,
//! with nothing consumed on the way.
//!
//! The ways on are taken in the order the C library takes them when it
//! walks the path of a match: at a choice the first way that leads to
//! the match, unless the walk has been through the instruction that way
//! begins with since it last consumed a byte; then the second, when
//! that leads to the match too. Which ways lead to the match is not
//! known here, so each is followed, the one the C library prefers
//! first: a way followed later is the walk the C library takes when
//! every way before it led nowhere.

use super::{Cache, MAX_LIST, Threads, holds};
use crate::program::{Inst, Program};
use crate::registers::{close, room};

/// The most times at one position of the text that a path is let
/// through an instruction it is on already. The C library goes round a
/// repetition of things that may match nothing once for each of them;
/// past this many steps the rounds are cut short and the groups of
/// such a repetition are those of fewer rounds.
pub const ROUNDS: u32 = 1 << 16;

pub enum Frame {
    /// an instruction, and whether a condition was met on the way
    Explore(u32, bool),
    Restore(u32, usize),
    /// the path leaves an instruction it came to for the first time
    Leave(u32),
    /// and one it came back to, which was this deep before
    Back(u32, u32),
}

/// The most frames a search of `prog` with `width` slots stacks up.
fn frames(prog: &Program, width: usize) -> usize {
    let again = if prog.cyclic { ROUNDS as usize } else { 0 };
    let each = 1 + usize::from(prog.cyclic) + usize::from(prog.snapshots && width > 2);
    each * prog.insts.len() + width + 4 * again + 2
}

/// One path followed from behind a byte, or from where a match begins.
pub struct Walk<'a> {
    prog: &'a Program,
    list: &'a mut Threads,
    seen: &'a mut [u32],
    passed: &'a mut [u32],
    depth: &'a mut [u32],
    stack: &'a mut Vec<Frame>,
    text: &'a [u8],
    turn: u32,
    captures: usize,
    /// the most slots the paths of one position carry
    most: usize,
    exhausted: &'a mut bool,
    /// the most frames the stack holds
    frames: usize,
    /// how many instructions the path is on
    size: u32,
    /// how often it may still come back to one
    rounds: &'a mut u32,
    /// The path came back to this instruction and led nowhere from
    /// there: the C library never leaves a walk it has begun, so what
    /// is followed until the path leaves the instruction is only what
    /// else can be reached, in any order.
    astray: Option<u32>,
}

impl<'a> Walk<'a> {
    /// A walk that adds to the paths of the position reached, `ahead`,
    /// or to those of the position the search stands at; and the slots
    /// it starts with.
    pub fn over(
        prog: &'a Program,
        cache: &'a mut Cache,
        text: &'a [u8],
        captures: usize,
        ahead: bool,
    ) -> (Self, &'a mut [usize]) {
        let list = match ahead {
            true => &mut cache.next,
            false => &mut cache.now,
        };
        let width = cache.scratch.len();
        let walk = Walk {
            prog,
            list,
            seen: &mut cache.seen,
            passed: &mut cache.passed,
            depth: &mut cache.depth,
            stack: &mut cache.stack,
            text,
            turn: cache.turn,
            captures,
            most: MAX_LIST.min((prog.row_count as usize + 1) * width),
            exhausted: &mut cache.exhausted,
            frames: frames(prog, width),
            size: 0,
            rounds: &mut cache.rounds,
            astray: None,
        };
        (walk, &mut cache.scratch)
    }

    /// Whether the path goes on from `pc`, which a path has come to in
    /// this turn already: only when it is this path, grown since.
    fn again(&mut self, pc: u32) -> bool {
        let deep = match self.depth.get(pc as usize) {
            Some(&deep) if deep != 0 && deep < self.size => deep,
            _ => return false,
        };
        if self.astray.is_some() || *self.rounds == 0 {
            return false;
        }
        *self.rounds -= 1;
        self.stack.push(Frame::Back(pc, deep));
        self.depth[pc as usize] = self.size;
        true
    }

    /// Marks `pc` as come to; false when the path ends there, waiting
    /// for a byte or as the match.
    fn arrive(&mut self, pc: u32, behind: bool, slots: &[usize]) -> bool {
        let waits = matches!(
            self.prog.insts[pc as usize],
            Inst::Set(..) | Inst::Byte(..) | Inst::Switch(..) | Inst::Match
        );
        if behind && matches!(self.prog.insts[pc as usize], Inst::Match) {
            self.passed[pc as usize] = self.turn;
            self.list.late = true;
            self.list.last.clear();
            self.list.last.extend_from_slice(slots);
            return false;
        }
        match behind && !waits {
            true => self.passed[pc as usize] = self.turn,
            false => self.seen[pc as usize] = self.turn,
        }
        if waits && self.list.slots.len() + slots.len() > MAX_LIST {
            *self.exhausted = true;
        } else if waits {
            room(&mut self.list.slots, slots.len(), self.most);
            self.list.order.push(pc);
            self.list.slots.extend_from_slice(slots);
        } else if self.prog.cyclic {
            self.size += 1;
            self.depth[pc as usize] = self.size;
            self.stack.push(Frame::Leave(pc));
        }
        !waits
    }

    /// Takes back what a frame stands for; an instruction to go on
    /// from is handed back.
    fn unwind(&mut self, frame: Frame, slots: &mut [usize]) -> Option<(u32, bool)> {
        match frame {
            Frame::Explore(pc, behind) => return Some((pc, behind)),
            Frame::Restore(slot, old) => slots[slot as usize] = old,
            Frame::Leave(pc) => {
                self.depth[pc as usize] = 0;
                self.size -= 1;
                if self.astray == Some(pc) {
                    self.astray = None;
                }
            }
            Frame::Back(pc, deep) => {
                self.depth[pc as usize] = deep;
                self.astray.get_or_insert(pc);
            }
        }
        None
    }

    /// Adds the path at `pc`, and every path reachable from it without
    /// consuming a byte, to the list; `at` is where the text stands.
    pub fn follow(&mut self, slots: &mut [usize], pc: u32, at: usize) {
        let prog = self.prog;
        self.stack.push(Frame::Explore(pc, false));
        while let Some(frame) = self.stack.pop() {
            let Some((pc, behind)) = self.unwind(frame, slots) else {
                continue;
            };
            // what one instruction stacks up: its ways on, its leaving
            // and every slot it writes
            room(self.stack, 2 * slots.len() + 4, self.frames);
            let came = self.seen[pc as usize] == self.turn
                || (behind && self.passed[pc as usize] == self.turn);
            let on = match came {
                true => self.again(pc),
                false => self.arrive(pc, behind, slots),
            };
            if !on {
                continue;
            }
            match prog.insts[pc as usize] {
                Inst::Split(first, second) => {
                    // the first way was taken on this path already
                    let turned = self.depth.get(first as usize).is_some_and(|&d| d != 0);
                    let (first, second) = match turned {
                        true => (second, first),
                        false => (first, second),
                    };
                    self.stack.push(Frame::Explore(second, behind));
                    self.stack.push(Frame::Explore(first, behind));
                }
                Inst::Save(slot, next) | Inst::SaveOpt(slot, next) => {
                    let slot = slot as usize;
                    if slot < self.captures {
                        let opt = matches!(prog.insts[pc as usize], Inst::SaveOpt(..));
                        let stack = &mut *self.stack;
                        close(slots, self.captures, slot, at, opt, |slot, old| {
                            stack.push(Frame::Restore(slot as u32, old))
                        });
                    }
                    self.stack.push(Frame::Explore(next, behind));
                }
                Inst::Look(look, next) if holds(look, self.text, at, prog.utf8) => {
                    self.stack.push(Frame::Explore(next, prog.mixed_ends));
                }
                // a pattern with one is not searched this way, and the
                // others were waited at
                _ => {}
            }
        }
    }
}
