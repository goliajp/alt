//! The pattern as a nondeterministic automaton: instructions a matcher
//! steps through, several at once.

use crate::Error;
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
    Look(Look, u32),
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
    /// Bytes no instruction tells apart share a class.
    pub classes: [u8; 256],
    /// One byte of each class.
    pub members: Vec<u8>,
}

/// More instructions than this is refused.
const MAX_INSTS: usize = 400_000;

struct Compiler {
    insts: Vec<Inst>,
    sets: Vec<ByteSet>,
}

pub fn compile(node: &Node, groups: u32) -> Result<Program, Error> {
    let mut c = Compiler {
        insts: Vec::new(),
        sets: Vec::new(),
    };
    let end = c.push(Inst::Match)?;
    let close = c.push(Inst::Save(1, end))?;
    let body = c.node(node, close)?;
    let start = c.push(Inst::Save(0, body))?;
    let plain_looks = c.insts.iter().all(|inst| match inst {
        Inst::Look(look, _) => matches!(look, Look::TextStart | Look::TextEnd),
        _ => true,
    });
    let (classes, members) = classes(&c.insts, &c.sets);
    Ok(Program {
        insts: c.insts,
        sets: c.sets,
        start,
        groups,
        anchored: anchored(node),
        plain_looks,
        classes,
        members,
    })
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

fn classes(insts: &[Inst], sets: &[ByteSet]) -> ([u8; 256], Vec<u8>) {
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
        match node {
            Node::Empty => Ok(next),
            Node::Set(set) => self.set(set, next),
            Node::Look(look) => self.push(Inst::Look(*look, next)),
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

    fn repeat(
        &mut self,
        inner: &Node,
        min: u32,
        max: Option<u32>,
        next: u32,
    ) -> Result<u32, Error> {
        let mut entry = match max {
            // the loop: once more, or on
            None => {
                let split = self.push(Inst::Split(next, next))?;
                let body = self.node(inner, split)?;
                self.insts[split as usize] = Inst::Split(body, next);
                split
            }
            // the optional copies, each reachable only through the one before
            Some(max) => {
                let mut entry = next;
                for _ in min..max {
                    let body = self.node(inner, entry)?;
                    entry = self.push(Inst::Split(body, next))?;
                }
                entry
            }
        };
        for _ in 0..min {
            entry = self.node(inner, entry)?;
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
        compile(&node, groups).unwrap()
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
        assert!(matches!(compile(&node, groups), Err(Error::TooBig)));
        assert!(!program("\\bx").plain_looks && program("^x$").plain_looks);
    }
}
