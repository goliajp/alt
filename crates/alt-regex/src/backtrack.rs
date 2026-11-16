//! The groups of a match whose two ends are known: the paths of the
//! automaton from one end are tried in order of preference, and the first
//! that arrives at the other end is the one the step-by-step matcher
//! would have kept. A path is never tried twice from the same position
//! at an instruction where paths meet, which bounds the work by pattern
//! times text.

use crate::pike::holds;
use crate::program::{Inst, NO_JOIN, Program};
use crate::registers::close;

/// The most bits of visited marks a search may need.
const MAX_MARKS: usize = 1 << 21;

/// The most choices and slots written a search may leave behind it,
/// 4 MiB of them: one that could leave more is left to the search that
/// steps every path at once.
const MAX_FRAMES: usize = 1 << 18;

enum Frame {
    Step(u32, usize),
    Restore(u32, usize),
}

/// Storage a search reuses.
#[derive(Default)]
pub struct Cache {
    visited: Vec<u64>,
    stack: Vec<Frame>,
    /// the slots with their snapshot behind them, for a program that
    /// keeps one
    both: Vec<usize>,
}

/// Whether a match of `len` bytes is one to search this way: small
/// enough, and of a pattern whose paths never come back to where they
/// were. An instruction is come to once at each position, through the
/// one way into it or as a join not yet marked, so what is left behind
/// is no more than the instructions that leave something times the
/// positions.
pub fn fits(prog: &Program, len: usize) -> bool {
    !prog.cyclic
        && prog.join_count as usize * (len + 1) <= MAX_MARKS
        && prog.left_behind as usize * (len + 1) <= MAX_FRAMES
}

/// Fills `slots` with the capture slots of the match from `from` to `to`,
/// which the pattern is known to have; `None` when a path grew longer
/// than this search keeps.
pub fn captures(
    prog: &Program,
    cache: &mut Cache,
    text: &[u8],
    from: usize,
    to: usize,
    slots: &mut [usize],
) -> Option<bool> {
    let captures = slots.len();
    if !prog.snapshots {
        return preferred(prog, cache, text, from, to, slots, captures);
    }
    let mut both = std::mem::take(&mut cache.both);
    both.clear();
    both.resize(2 * captures, crate::pike::UNSET);
    let found = preferred(prog, cache, text, from, to, &mut both, captures);
    slots.copy_from_slice(&both[..captures]);
    cache.both = both;
    found
}

/// The path the C library keeps: one that ends without a condition
/// behind its last byte, when the pattern has both kinds and the match
/// has such a path.
fn preferred(
    prog: &Program,
    cache: &mut Cache,
    text: &[u8],
    from: usize,
    to: usize,
    slots: &mut [usize],
    captures: usize,
) -> Option<bool> {
    if prog.mixed_ends && paths(prog, cache, text, from, to, slots, captures, true)? {
        return Some(true);
    }
    paths(prog, cache, text, from, to, slots, captures, false)
}

/// With `plain`, a path that meets a condition at the end of the match
/// is given up.
#[allow(clippy::too_many_arguments)]
fn paths(
    prog: &Program,
    cache: &mut Cache,
    text: &[u8],
    from: usize,
    to: usize,
    slots: &mut [usize],
    captures: usize,
    plain: bool,
) -> Option<bool> {
    let span = to - from + 1;
    let Cache { visited, stack, .. } = cache;
    visited.clear();
    visited.resize((prog.join_count as usize * span).div_ceil(64), 0);
    stack.clear();
    stack.push(Frame::Step(prog.start, from));
    while let Some(frame) = stack.pop() {
        let (mut pc, mut at) = match frame {
            Frame::Restore(slot, old) => {
                slots[slot as usize] = old;
                continue;
            }
            Frame::Step(pc, at) => (pc, at),
        };
        loop {
            let join = prog.joins[pc as usize];
            if join != NO_JOIN {
                let mark = join as usize * span + (at - from);
                let (word, bit) = (mark / 64, 1u64 << (mark % 64));
                if visited[word] & bit != 0 {
                    break;
                }
                visited[word] |= bit;
            }
            match prog.insts[pc as usize] {
                Inst::Byte(b, next) if at < to && text[at] == b => (pc, at) = (next, at + 1),
                Inst::Set(set, next) if at < to && prog.sets[set as usize].contains(text[at]) => {
                    (pc, at) = (next, at + 1)
                }
                Inst::Switch(copied, place) if at < to => {
                    match prog.taken(copied, place, text[at]) {
                        Some(target) => (pc, at) = (target, at + 1),
                        None => break,
                    }
                }
                Inst::Split(first, second) => {
                    stack.push(Frame::Step(second, at));
                    pc = first;
                }
                Inst::Save(slot, next) | Inst::SaveOpt(slot, next) => {
                    let slot = slot as usize;
                    // most programs keep no snapshot: one slot is written
                    if slot < captures && slots.len() == captures {
                        stack.push(Frame::Restore(slot as u32, slots[slot]));
                        slots[slot] = at;
                    } else if slot < captures {
                        // a snapshot taken or put back writes every slot
                        if stack.len() + slots.len() > MAX_FRAMES {
                            return None;
                        }
                        let opt = matches!(prog.insts[pc as usize], Inst::SaveOpt(..));
                        close(slots, captures, slot, at, opt, |slot, old| {
                            stack.push(Frame::Restore(slot as u32, old))
                        });
                    }
                    pc = next;
                }
                Inst::Look(look, next)
                    if !(plain && at == to) && holds(look, text, at, prog.utf8) =>
                {
                    pc = next
                }
                Inst::Match if at == to => return Some(true),
                _ => break,
            }
        }
    }
    Some(false)
}
