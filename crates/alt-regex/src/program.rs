//! The pattern as a nondeterministic automaton: instructions a matcher
//! steps through, several at once.

use crate::analysis::{anchored, classes, counts, joins, mixed_ends};
use std::sync::Arc;

use crate::block::{Block, EXIT};
use crate::budget::Budget;
use crate::class::ByteSet;
use crate::syntax::{Look, Node, Shape};
use crate::{Error, Options};

mod repeat;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Inst {
    /// Consume one byte of the set with this index.
    Set(u32, u32),
    /// Consume one byte of a character, at this place of the block
    /// copied in with this number: see [`Program::taken`].
    Switch(u32, u32),
    /// Consume this byte.
    Byte(u8, u32),
    /// Go on at either target; the first is preferred.
    Split(u32, u32),
    /// Record the position in a capture slot.
    Save(u32, u32),
    /// Record the end of a group that a repetition may leave out and
    /// that may match nothing: see [`crate::registers::close`].
    SaveOpt(u32, u32),
    Look(Look, u32),
    /// Consume what this group matched, letters in either case if asked.
    BackRef(u32, bool, u32),
    Match,
}

/// A set of characters copied into a program: its places are the
/// instructions from `base` on, and behind a character comes `next`.
pub struct Copied {
    block: Arc<Block>,
    base: u32,
    pub next: u32,
    /// the first place, and the characters of one byte
    start: u32,
    ascii: ByteSet,
}

impl Program {
    /// Where the instruction at place `place` of the copy `copied` goes
    /// on byte `b`.
    #[inline]
    pub fn taken(&self, copied: u32, place: u32, b: u8) -> Option<u32> {
        let copied = &self.blocks[copied as usize];
        // most text is ASCII, which only the first place takes
        if b < 0x80 {
            let taken = place == copied.start && copied.ascii.contains(b);
            return taken.then_some(copied.next);
        }
        match copied.block.places[place as usize].on(b)? {
            EXIT => Some(copied.next),
            place => Some(copied.base + place),
        }
    }
}

pub struct Program {
    pub insts: Vec<Inst>,
    pub sets: Vec<ByteSet>,
    pub start: u32,
    pub groups: u32,
    /// The text is UTF-8, matched character by character.
    pub utf8: bool,
    /// No match begins inside a character.
    pub apart: bool,
    /// The sets of characters copied in.
    pub blocks: Vec<Copied>,
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
    /// Something that may match nothing is repeated without bound: a
    /// path can come back to an instruction with no byte consumed.
    pub cyclic: bool,
    /// A match can end behind a condition and without one: see
    /// [`mixed_ends`].
    pub mixed_ends: bool,
    /// For each instruction that more than one path leads into, its
    /// number among those; the others have none.
    pub joins: Vec<u32>,
    pub join_count: u32,
    /// How many instructions a path waits at, for a byte or as the match.
    pub row_count: u32,
    /// How many leave something behind to come back to.
    pub left_behind: u32,
    /// Bytes no instruction tells apart share a class.
    pub classes: [u8; 256],
    /// One byte of each class.
    pub members: Vec<u8>,
}

/// More instructions than this is refused.
pub const MAX_INSTS: usize = 400_000;
/// More steps through the tree of the pattern than this is refused.
const MAX_WORK: usize = 4 * MAX_INSTS;
/// The bytes held beside each instruction: its join, how the match is
/// reached from it, and the mark a search keeps of it.
const BESIDE: usize = 9;

struct Compiler {
    insts: Vec<Inst>,
    sets: Vec<ByteSet>,
    work: usize,
    /// the bytes the tree and the program so far hold
    budget: Budget,
    ignore_case: bool,
    /// inside a copy of a repeated expression other than its first
    copied: bool,
    cyclic: bool,
    blocks: Vec<Copied>,
    /// each block whose sets are among the program's
    met: Vec<*const Block>,
}

pub fn compile(node: &Node, groups: u32, options: Options, shape: Shape) -> Result<Program, Error> {
    let mut c = Compiler {
        insts: Vec::new(),
        sets: Vec::new(),
        work: 0,
        budget: shape.budget,
        ignore_case: options.ignore_case,
        copied: false,
        cyclic: repeat::cyclic(node),
        blocks: Vec::new(),
        met: Vec::new(),
    };
    let end = c.push(Inst::Match)?;
    let close = c.push(Inst::Save(1, end))?;
    let body = c.node(node, close)?;
    let start = c.push(Inst::Save(0, body))?;
    let plain_looks = c.insts.iter().all(|inst| match inst {
        Inst::Look(look, _) => matches!(look, Look::TextStart | Look::TextEnd),
        _ => true,
    });
    // the tables begin a match at any byte
    let plain_looks = plain_looks && !(shape.apart && shape.strays);
    let (row_count, left_behind) = counts(&c.insts);
    let (classes, members) = classes(&c.insts, &c.sets);
    let exits: Vec<u32> = c.blocks.iter().map(|copied| copied.next).collect();
    let (joins, join_count) = joins(&c.insts, start, &exits);
    let mixed_ends = mixed_ends(&c.insts, start, &exits);
    Ok(Program {
        backrefs: c.insts.iter().any(|i| matches!(i, Inst::BackRef(..))),
        snapshots: c.insts.iter().any(|i| matches!(i, Inst::SaveOpt(..))),
        insts: c.insts,
        sets: c.sets,
        start,
        groups,
        utf8: options.utf8,
        blocks: c.blocks,
        apart: shape.apart,
        anchored: anchored(node),
        plain_looks,
        cyclic: c.cyclic,
        mixed_ends,
        joins,
        join_count,
        row_count,
        left_behind,
        classes,
        members,
    })
}

/// Whether `node` can match without consuming a byte.
fn nullable(node: &Node) -> bool {
    match node {
        Node::Empty | Node::Look(_) | Node::BackRef(_) => true,
        Node::Set(_) | Node::Class(_) => false,
        Node::Group(inner, _) => nullable(inner),
        Node::Concat(items) => items.iter().all(nullable),
        Node::Alt(branches) => branches.iter().any(nullable),
        Node::Repeat(inner, min, _) => *min == 0 || nullable(inner),
    }
}

/// No number: the instruction is reached one way only.
pub const NO_JOIN: u32 = u32::MAX;

impl Compiler {
    fn push(&mut self, inst: Inst) -> Result<u32, Error> {
        if self.insts.len() >= MAX_INSTS {
            return Err(Error::TooBig);
        }
        self.budget.push(&mut self.insts, inst)?;
        self.budget.take(BESIDE)?;
        Ok(u32::try_from(self.insts.len() - 1).expect("below the limit"))
    }

    fn set(&mut self, set: &ByteSet, next: u32) -> Result<u32, Error> {
        if let Some(b) = set.single() {
            return self.push(Inst::Byte(b, next));
        }
        let index = match self.sets.iter().position(|s| s == set) {
            Some(known) => known,
            None => {
                self.budget.push(&mut self.sets, *set)?;
                self.sets.len() - 1
            }
        };
        self.push(Inst::Set(u32::try_from(index).expect("few sets"), next))
    }

    /// Copies the places of `block` in, to go on at `next`.
    fn block(&mut self, block: &Arc<Block>, next: u32) -> Result<u32, Error> {
        self.work += block.places.len();
        if self.insts.len() + block.places.len() > MAX_INSTS || self.work > MAX_WORK {
            return Err(Error::TooBig);
        }
        let pointer = Arc::as_ptr(block);
        let room = self.sets.capacity() * size_of::<ByteSet>();
        if !self.met.contains(&pointer) {
            self.sets.extend(&block.sets);
            self.budget.push(&mut self.met, pointer)?;
        }
        let base = u32::try_from(self.insts.len()).expect("below the limit");
        let number = u32::try_from(self.blocks.len()).expect("below the limit");
        let places = 0..block.places.len() as u32;
        let room = room + self.insts.capacity() * size_of::<Inst>();
        self.insts
            .extend(places.map(|place| Inst::Switch(number, place)));
        let grown =
            self.sets.capacity() * size_of::<ByteSet>() + self.insts.capacity() * size_of::<Inst>();
        self.budget
            .take(grown - room + block.places.len() * BESIDE)?;
        let copied = Copied {
            block: block.clone(),
            base,
            next,
            start: block.start,
            ascii: block.ascii,
        };
        self.budget.push(&mut self.blocks, copied)?;
        Ok(base + block.start)
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
            Node::Class(block) => self.block(block, next),
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
            // nested to the left, as the tree of the C library is: where
            // a path comes back to a choice, which way it then takes
            // depends on it
            Node::Alt(branches) if self.cyclic => {
                let mut entry = self.node(&branches[0], next)?;
                for branch in &branches[1..] {
                    let other = self.node(branch, next)?;
                    entry = self.push(Inst::Split(entry, other))?;
                }
                Ok(entry)
            }
            // elsewhere one choice is met before each branch
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Options;
    use crate::syntax::parse;

    fn program(pat: &str) -> Program {
        let (node, groups) = parse(pat.as_bytes(), Options::default()).unwrap();
        compile(&node, groups, Options::default(), Shape::default()).unwrap()
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
        assert!(matches!(
            compile(&node, groups, Options::default(), Shape::default()),
            Err(Error::TooBig)
        ));
        assert!(!program("\\bx").plain_looks && program("^x$").plain_looks);
    }
}
