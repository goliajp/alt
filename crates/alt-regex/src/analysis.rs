//! What a matcher needs to know about the instructions before it runs.

use crate::class::ByteSet;
use crate::program::{Inst, NO_JOIN};

/// Whether a match can end both ways: behind a condition with nothing
/// consumed after it, and without one. The C library keeps the second
/// kind of path when it has both, whichever the pattern prefers.
pub fn mixed_ends(insts: &[Inst], start: u32) -> bool {
    const PLAIN: u8 = 1;
    const BEHIND: u8 = 2;
    // how the match is reached from each instruction with nothing consumed
    let mut reach = vec![0u8; insts.len()];
    let (mut plain, mut behind) = (false, false);
    loop {
        let mut grew = false;
        for (pc, inst) in insts.iter().enumerate() {
            let now = match *inst {
                Inst::Match => PLAIN,
                Inst::Save(_, next) | Inst::SaveOpt(_, next) => reach[next as usize],
                Inst::Split(first, second) => reach[first as usize] | reach[second as usize],
                Inst::Look(_, next) if reach[next as usize] != 0 => BEHIND,
                Inst::Look(..) => 0,
                Inst::Set(_, next) | Inst::Byte(_, next) | Inst::BackRef(_, _, next) => {
                    plain |= reach[next as usize] & PLAIN != 0;
                    behind |= reach[next as usize] & BEHIND != 0;
                    0
                }
            };
            grew |= now != reach[pc];
            reach[pc] = now;
        }
        if !grew {
            break;
        }
    }
    let whole = reach[start as usize];
    (plain || whole & PLAIN != 0) && (behind || whole & BEHIND != 0)
}

pub fn joins(insts: &[Inst], start: u32) -> (Vec<u32>, u32) {
    let mut ways = vec![0u32; insts.len()];
    ways[start as usize] = 1;
    for inst in insts {
        match *inst {
            Inst::Set(_, next)
            | Inst::Byte(_, next)
            | Inst::Save(_, next)
            | Inst::SaveOpt(_, next)
            | Inst::BackRef(_, _, next)
            | Inst::Look(_, next) => ways[next as usize] += 1,
            Inst::Split(first, second) => {
                ways[first as usize] += 1;
                ways[second as usize] += 1;
            }
            Inst::Match => {}
        }
    }
    let mut count = 0;
    let numbered = |ways: u32| match ways > 1 {
        true => {
            count += 1;
            count - 1
        }
        false => NO_JOIN,
    };
    (ways.into_iter().map(numbered).collect(), count)
}

pub fn rows(insts: &[Inst]) -> (Vec<u32>, u32) {
    let mut count = 0;
    let numbered = |inst: &Inst| match inst {
        Inst::Set(..) | Inst::Byte(..) | Inst::Match => {
            count += 1;
            count - 1
        }
        _ => NO_JOIN,
    };
    (insts.iter().map(numbered).collect(), count)
}

pub fn classes(insts: &[Inst], sets: &[ByteSet]) -> ([u8; 256], Vec<u8>) {
    let mut singles = ByteSet::empty();
    for inst in insts {
        if let Inst::Byte(b, _) = inst {
            singles.insert(*b);
        }
    }
    let mut seen: Vec<(Vec<bool>, bool, u8)> = Vec::new();
    let mut classes = [0u8; 256];
    let mut members = Vec::new();
    for b in 0..=255u8 {
        // a byte matched on its own is a class of its own
        let own = singles.contains(b);
        let key: Vec<bool> = sets.iter().map(|set| set.contains(b)).collect();
        let known = seen
            .iter()
            .position(|(k, single, _)| !own && !*single && *k == key);
        let class = known.unwrap_or_else(|| {
            seen.push((key, own, b));
            members.push(b);
            seen.len() - 1
        });
        classes[usize::from(b)] = u8::try_from(class).expect("at most 256 classes");
    }
    (classes, members)
}
