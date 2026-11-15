//! The pattern as a nondeterministic automaton: instructions a matcher
//! steps through, several at once.

use crate::Error;
use crate::analysis::{classes, joins, mixed_ends, rows};
use crate::class::ByteSet;
use crate::syntax::{Look, Node};

#[derive(Clone, Copy, Debug)]
pub enum Inst {
    /// Consume one byte of the set with this index.
    Set(u32, u32),
    /// Consume this byte.
    Byte(u8, u32),
    /// Go on at either target; the first is preferred.
    Split(u32, u32),
    /// Record the position in a capture slot.
    Save(u32, u32),
    /// Record the end of a group that a repetition may leave out and
    /// that may match nothing: see [`close`].
    SaveOpt(u32, u32),
    Look(Look, u32),
    /// Consume what this group matched, letters in either case if asked.
    BackRef(u32, bool, u32),
    Match,
}

pub struct Program {
    pub insts: Vec<Inst>,
    pub sets: Vec<ByteSet>,
    pub start: u32,
    pub groups: u32,
    /// Every match begins at the start of the text.
    pub anchored: bool,
    /// The only conditions are on the two ends of the text.
    pub plain_looks: bool,
    /// Some instruction is a back-reference.
    pub backrefs: bool,
    /// Some group ends in [`Inst::SaveOpt`]: a path carries, behind its
    /// capture slots, what they were when a group last closed around
    /// something.
    pub snapshots: bool,
    /// A match can end behind a condition and without one: see
    /// [`mixed_ends`].
    pub mixed_ends: bool,
    /// For each instruction that more than one path leads into, its
    /// number among those; the others have none.
    pub joins: Vec<u32>,
    pub join_count: u32,
    /// For each instruction a path waits at, for a byte or as the match,
    /// its number among those.
    pub rows: Vec<u32>,
    pub row_count: u32,
    /// Bytes no instruction tells apart share a class.
    pub classes: [u8; 256],
    /// One byte of each class.
    pub members: Vec<u8>,
}

/// More instructions than this is refused.
const MAX_INSTS: usize = 400_000;
/// More steps through the tree of the pattern than this is refused.
const MAX_WORK: usize = 4 * MAX_INSTS;
/// The most capture slots the matcher that carries them may hold: one
/// row of all the slots for each instruction a path can wait at.
const MAX_SLOTS: usize = 1 << 22;

struct Compiler {
    insts: Vec<Inst>,
    sets: Vec<ByteSet>,
    work: usize,
    ignore_case: bool,
    /// inside a copy of a repeated expression other than its first
    copied: bool,
}

pub fn compile(node: &Node, groups: u32, ignore_case: bool) -> Result<Program, Error> {
    let mut c = Compiler {
        insts: Vec::new(),
        sets: Vec::new(),
        work: 0,
        ignore_case,
        copied: false,
    };
    let end = c.push(Inst::Match)?;
    let close = c.push(Inst::Save(1, end))?;
    let body = c.node(node, close)?;
    let start = c.push(Inst::Save(0, body))?;
    let plain_looks = c.insts.iter().all(|inst| match inst {
        Inst::Look(look, _) => matches!(look, Look::TextStart | Look::TextEnd),
        _ => true,
    });
    let backrefs = c.insts.iter().any(|i| matches!(i, Inst::BackRef(..)));
    let snapshots = c.insts.iter().any(|i| matches!(i, Inst::SaveOpt(..)));
    let (rows, row_count) = rows(&c.insts);
    if row_count as usize * 2 * (groups as usize + 1) > MAX_SLOTS {
        return Err(Error::TooBig);
    }
    let (classes, members) = classes(&c.insts, &c.sets);
    let (joins, join_count) = joins(&c.insts, start);
    let mixed_ends = mixed_ends(&c.insts, start);
    Ok(Program {
        insts: c.insts,
        sets: c.sets,
        start,
        groups,
        anchored: anchored(node),
        plain_looks,
        backrefs,
        snapshots,
        mixed_ends,
        joins,
        join_count,
        rows,
        row_count,
        classes,
        members,
    })
}

/// Records position `at` in capture slot `slot` of a path, as the C
/// library keeps its registers. `slots` holds the `width` capture slots
/// in use and, for a program with snapshots, as many again: what the
/// first were when a group last closed around something. A group that
/// closes around nothing, in a turn of a repetition that could have been
/// left out and behind an earlier match of the group, puts every slot
/// back to that.
pub fn close(prog: &Program, slots: &mut [usize], width: usize, slot: usize, at: usize, opt: bool) {
    if !prog.snapshots || slot.is_multiple_of(2) {
        slots[slot] = at;
        return;
    }
    let (now, kept) = slots.split_at_mut(width);
    if now[slot - 1] < at {
        now[slot] = at;
        kept.copy_from_slice(now);
    } else if opt && kept[slot - 1] != usize::MAX {
        now.copy_from_slice(kept);
    } else {
        now[slot] = at;
    }
}

/// Whether `node` can match without consuming a byte.
fn nullable(node: &Node) -> bool {
    match node {
        Node::Empty | Node::Look(_) | Node::BackRef(_) => true,
        Node::Set(_) => false,
        Node::Group(inner, _) => nullable(inner),
        Node::Concat(items) => items.iter().all(nullable),
        Node::Alt(branches) => branches.iter().any(nullable),
        Node::Repeat(inner, min, _) => *min == 0 || nullable(inner),
    }
}

/// Whether nothing can match before `node` except at the start of the text.
fn anchored(node: &Node) -> bool {
    match node {
        Node::Look(Look::TextStart) => true,
        Node::Group(inner, _) => anchored(inner),
        Node::Concat(items) => items.first().is_some_and(anchored),
        Node::Alt(branches) => branches.iter().all(anchored),
        Node::Repeat(inner, min, _) => *min > 0 && anchored(inner),
        _ => false,
    }
}

/// No number: the instruction is reached one way only.
pub const NO_JOIN: u32 = u32::MAX;

impl Compiler {
    fn push(&mut self, inst: Inst) -> Result<u32, Error> {
        if self.insts.len() >= MAX_INSTS {
            return Err(Error::TooBig);
        }
        self.insts.push(inst);
        Ok(u32::try_from(self.insts.len() - 1).expect("below the limit"))
    }

    fn set(&mut self, set: &ByteSet, next: u32) -> Result<u32, Error> {
        if let Some(b) = set.single() {
            return self.push(Inst::Byte(b, next));
        }
        let known = self.sets.iter().position(|s| s == set);
        let index = known.unwrap_or_else(|| {
            self.sets.push(*set);
            self.sets.len() - 1
        });
        self.push(Inst::Set(u32::try_from(index).expect("few sets"), next))
    }

    /// Compiles `node` to go on at `next`; returns where it begins.
    fn node(&mut self, node: &Node, next: u32) -> Result<u32, Error> {
        self.work += 1;
        if self.work > MAX_WORK {
            return Err(Error::TooBig);
        }
        match node {
            Node::Empty => Ok(next),
            Node::Set(set) => self.set(set, next),
            Node::Look(look) => self.push(Inst::Look(*look, next)),
            Node::BackRef(group) => self.push(Inst::BackRef(*group, self.ignore_case, next)),
            Node::Group(inner, number) => {
                let close = self.push(Inst::Save(2 * number + 1, next))?;
                let body = self.node(inner, close)?;
                self.push(Inst::Save(2 * number, body))
            }
            Node::Concat(items) => items
                .iter()
                .rev()
                .try_fold(next, |next, item| self.node(item, next)),
            Node::Alt(branches) => {
                let mut entries = Vec::with_capacity(branches.len());
                for branch in branches {
                    entries.push(self.node(branch, next)?);
                }
                let last = entries.pop().expect("an alternation has branches");
                entries
                    .into_iter()
                    .rev()
                    .try_fold(last, |rest, entry| self.push(Inst::Split(entry, rest)))
            }
            Node::Repeat(inner, min, max) => self.repeat(inner, *min, *max, next),
        }
    }

    /// One of the copies a repetition is made of. The C library marks
    /// a group that a repetition may leave out, the first such copy
    /// only, and a copy made of another loses the marks inside it.
    fn copy(&mut self, inner: &Node, next: u32, marked: bool, made: bool) -> Result<u32, Error> {
        let outside = self.copied;
        self.copied = outside || made;
        let entry = match inner {
            Node::Group(body, number) if marked && !outside && nullable(body) => {
                let close = self.push(Inst::SaveOpt(2 * number + 1, next))?;
                let body = self.node(body, close)?;
                self.push(Inst::Save(2 * number, body))
            }
            _ => self.node(inner, next),
        };
        self.copied = outside;
        entry
    }

    fn repeat(
        &mut self,
        inner: &Node,
        min: u32,
        max: Option<u32>,
        next: u32,
    ) -> Result<u32, Error> {
        let mut entry = match max {
            // the loop: once more, or on
            None if !nullable(inner) => {
                let split = self.push(Inst::Split(next, next))?;
                let body = self.copy(inner, split, false, min > 0)?;
                self.insts[split as usize] = Inst::Split(body, next);
                split
            }
            // a turn that takes nothing comes back to where it began, and
            // no path goes through an instruction twice at one place:
            // behind a turn the choice is made at an instruction of its
            // own, so that one such turn is taken and then left
            None => {
                let again = self.push(Inst::Split(next, next))?;
                let body = self.copy(inner, again, true, min > 0)?;
                self.insts[again as usize] = Inst::Split(body, next);
                self.push(Inst::Split(body, next))?
            }
            // the optional copies, wired as the C library wires them:
            // of fewer copies than there may be, the last ones are the
            // ones taken, each entered only behind the one before it
            Some(max) => {
                let mut bodies = Vec::new();
                let mut behind = next;
                for copy in (min..max).rev() {
                    let first = copy == min;
                    behind = self.copy(inner, behind, first, !first || min > 0)?;
                    bodies.push(behind);
                }
                bodies.reverse();
                let mut entry = next;
                for (at, body) in bodies.iter().enumerate() {
                    let without = bodies.get(at + 1).copied().unwrap_or(next);
                    let with = if at == 0 { *body } else { entry };
                    entry = self.push(Inst::Split(with, without))?;
                }
                entry
            }
        };
        for copy in (0..min).rev() {
            entry = self.copy(inner, entry, false, copy > 0)?;
        }
        Ok(entry)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Options;
    use crate::syntax::parse;

    fn program(pat: &str) -> Program {
        let (node, groups) = parse(pat.as_bytes(), Options::default()).unwrap();
        compile(&node, groups, false).unwrap()
    }

    #[test]
    fn a_pattern_is_anchored_when_every_way_into_it_is() {
        for (pat, anchored) in [
            ("^a", true),
            ("(^a|^b)c", true),
            ("(^a)+", true),
            ("^a|b", false),
            ("(^a)*b", false),
            ("a^", false),
        ] {
            assert_eq!(program(pat).anchored, anchored, "{pat}");
        }
    }

    #[test]
    fn bytes_the_pattern_treats_alike_share_a_class() {
        let p = program("[a-c]x[0-9]+");
        let class = |b: u8| p.classes[usize::from(b)];
        assert_eq!(class(b'a'), class(b'c'));
        assert_eq!(class(b'0'), class(b'9'));
        assert_ne!(class(b'a'), class(b'x'));
        assert_ne!(class(b'x'), class(b'y'));
        assert_eq!(class(b'y'), class(b'!'));
        assert_eq!(p.members.len(), 4);
        for (class, &member) in p.members.iter().enumerate() {
            assert_eq!(usize::from(p.classes[usize::from(member)]), class);
        }
    }

    #[test]
    fn counted_repetitions_are_unrolled_and_bounded() {
        let small = program("a{3}").insts.len();
        let large = program("a{30}").insts.len();
        assert_eq!(large - small, 27);
        assert!(program("a{2,4}").insts.len() > small);
        let (node, groups) = parse(b"((a{1000}){1000}){10}", Options::default()).unwrap();
        assert!(matches!(compile(&node, groups, false), Err(Error::TooBig)));
        assert!(!program("\\bx").plain_looks && program("^x$").plain_looks);
    }
}
