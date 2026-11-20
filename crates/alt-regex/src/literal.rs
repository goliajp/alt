//! Bytes every match must contain, found before any automaton runs.

use std::sync::OnceLock;

use alt_bytes::ByteSlice;

use crate::class::ByteSet;
use crate::program::{Inst, Program};
use crate::syntax::Node;

/// Fixed bytes to look for in a text. Where they lie is found by
/// looking for their first byte and comparing the rest, which is
/// fastest while it comes to something; a text that makes it compare
/// much in vain is read on byte by byte, each of them once.
pub struct Fixed {
    bytes: Vec<u8>,
    /// Where bytes are looked for without telling some of them apart:
    /// what each byte of a text counts as among `bytes`.
    alike: Option<Box<[u8; 256]>>,
    /// For each count of bytes the text may end with, how many it
    /// still ends with when the next byte is another: made when a
    /// search first reads byte by byte.
    kept: OnceLock<Vec<u32>>,
}

impl std::ops::Deref for Fixed {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        &self.bytes
    }
}

impl Fixed {
    /// `bytes` is not empty.
    pub fn new(bytes: Vec<u8>) -> Self {
        let (alike, kept) = (None, OnceLock::new());
        Fixed { bytes, alike, kept }
    }

    /// Where the bytes first lie in `text`.
    #[inline]
    pub fn find(&self, text: &[u8]) -> Option<usize> {
        self.sought(text, &mut 0)
    }

    /// As [`Fixed::find`]; `compared` counts the bytes compared beyond
    /// the first of each place.
    #[inline]
    pub fn sought(&self, text: &[u8], compared: &mut usize) -> Option<usize> {
        let (&first, rest) = self.bytes.split_first()?;
        let mut from = 0;
        while let Some(skipped) = text[from..].find_byte(first) {
            let at = from + skipped;
            let behind = &text[at + 1..];
            if behind.starts_with(rest) {
                return Some(at);
            }
            *compared += behind.iter().zip(rest).take_while(|(a, b)| a == b).count();
            from = at + 1;
            if *compared > 16 * from + 64 {
                return self.lies(text, from).next();
            }
        }
        None
    }

    /// The places from `from` on where the bytes lie, across one
    /// another too, each byte of the text read once.
    pub fn lies<'a>(&'a self, text: &'a [u8], from: usize) -> Lies<'a> {
        let kept = self.kept.get_or_init(|| kept(&self.bytes));
        let (at, held) = (from, 0);
        Lies {
            fixed: self,
            kept,
            text,
            at,
            held,
        }
    }
}

/// How many bytes of `fixed` a text still ends with, for each count it
/// ended with before a byte that does not go on.
fn kept(fixed: &[u8]) -> Vec<u32> {
    let mut kept = vec![0u32; fixed.len() + 1];
    let mut held = 0;
    for at in 1..fixed.len() {
        while held > 0 && fixed[at] != fixed[held] {
            held = kept[held] as usize;
        }
        held += usize::from(fixed[at] == fixed[held]);
        kept[at + 1] = held as u32;
    }
    kept
}

/// See [`Fixed::lies`].
pub struct Lies<'a> {
    fixed: &'a Fixed,
    kept: &'a [u32],
    text: &'a [u8],
    /// the next byte of the text to read
    at: usize,
    /// how many of the bytes the text read so far ends with
    held: usize,
}

impl Iterator for Lies<'_> {
    type Item = usize;

    fn next(&mut self) -> Option<usize> {
        let fixed = self.fixed.bytes.as_slice();
        while let Some(&b) = self.text.get(self.at) {
            let b = match &self.fixed.alike {
                Some(alike) => alike[usize::from(b)],
                None => b,
            };
            self.at += 1;
            while self.held > 0 && fixed[self.held] != b {
                self.held = self.kept[self.held] as usize;
            }
            self.held += usize::from(fixed[self.held] == b);
            if self.held == fixed.len() {
                self.held = self.kept[self.held] as usize;
                return Some(self.at - fixed.len());
            }
        }
        None
    }
}

/// What every match of `prog` begins with, and the instruction behind
/// it: a run of bytes, each one of a few that count alike, as the
/// letters of a pattern do that is matched without regard to case.
/// The run ends where a set of bytes lies across those before it, or
/// the paths part.
pub fn begins(prog: &Program) -> Option<(Fixed, u32)> {
    const NONE: u8 = u8::MAX;
    let mut alike = Box::new([NONE; 256]);
    // how many bytes count as each
    let mut sizes = Vec::new();
    let mut run = Vec::new();
    let mut pc = prog.start;
    while run.len() < prog.insts.len() {
        let (set, next) = match prog.insts[pc as usize] {
            Inst::Save(_, next) | Inst::SaveOpt(_, next) => {
                pc = next;
                continue;
            }
            Inst::Byte(b, next) => (ByteSet::byte(b), next),
            Inst::Set(set, next) => (prog.sets[set as usize], next),
            Inst::Switch(copied, place) => {
                let mut set = ByteSet::empty();
                let mut targets = (0..=u8::MAX).filter_map(|b| {
                    let target = prog.taken(copied, place, b)?;
                    set.insert(b);
                    Some(target)
                });
                let Some(first) = targets.next() else { break };
                if targets.any(|target| target != first) {
                    break;
                }
                (set, first)
            }
            _ => break,
        };
        let mut held = ByteSet::empty();
        let mut known = None;
        set.each(|b| {
            held.insert(alike[usize::from(b)]);
            known = Some(alike[usize::from(b)]);
        });
        let as_one = match known.filter(|_| held.len() == 1) {
            Some(NONE) if sizes.len() < usize::from(NONE) => {
                set.each(|b| alike[usize::from(b)] = sizes.len() as u8);
                sizes.push(set.len());
                sizes.len() as u8 - 1
            }
            Some(one) if one != NONE && sizes[usize::from(one)] == set.len() => one,
            _ => break,
        };
        run.push(as_one);
        pc = next;
    }
    let lead = Fixed {
        bytes: run,
        alike: Some(alike),
        kept: OnceLock::new(),
    };
    (!lead.is_empty()).then_some((lead, pc))
}

/// The longest run of fixed bytes that every match of `node` contains.
pub fn required(node: &Node) -> Option<Vec<u8>> {
    let mut best = Vec::new();
    let mut run = Vec::new();
    walk(node, &mut run, &mut best);
    close(&mut run, &mut best);
    (!best.is_empty()).then_some(best)
}

/// Whether `node` is fixed bytes and nothing else: where they lie in a
/// text is then where it matches.
pub fn alone(node: &Node) -> bool {
    let fixed = |node: &Node| matches!(node, Node::Set(set) if set.single().is_some());
    match node {
        Node::Concat(items) => items.iter().all(fixed),
        _ => fixed(node),
    }
}

/// Whether every match of `node` begins with the bytes `fixed`, as
/// far as the pattern says so in as many words: `fixed` is the run of
/// fixed bytes it begins with.
pub fn leads(node: &Node, fixed: &[u8]) -> bool {
    let mut run = Vec::new();
    lead(node, &mut run);
    run == fixed
}

/// Adds the fixed bytes `node` begins with to `run`; false when
/// something else may come behind them.
fn lead(node: &Node, run: &mut Vec<u8>) -> bool {
    match node {
        Node::Set(set) => set.single().map(|b| run.push(b)).is_some(),
        Node::Empty => true,
        Node::Group(inner, _) => lead(inner, run),
        Node::Concat(items) => items.iter().all(|item| lead(item, run)),
        _ => false,
    }
}

fn close(run: &mut Vec<u8>, best: &mut Vec<u8>) {
    if run.len() > best.len() {
        std::mem::swap(run, best);
    }
    run.clear();
}

fn walk(node: &Node, run: &mut Vec<u8>, best: &mut Vec<u8>) {
    match node {
        Node::Set(set) => match set.single() {
            Some(b) => run.push(b),
            None => close(run, best),
        },
        // these consume nothing: the bytes around them stay adjacent
        Node::Empty | Node::Look(_) => {}
        Node::Group(inner, _) => walk(inner, run, best),
        Node::Concat(items) => items.iter().for_each(|item| walk(item, run, best)),
        Node::Repeat(inner, min, _) if *min > 0 => {
            close(run, best);
            walk(inner, run, best);
            close(run, best);
        }
        Node::Alt(_) | Node::Repeat(..) | Node::BackRef(_) | Node::Class(_) => close(run, best),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Options;
    use crate::syntax::parse;

    #[test]
    fn the_longest_fixed_run_outside_choices_is_required() {
        for (pat, literal) in [
            ("^[ \t]*(class|struct) x", Some(" x")),
            ("foo(bar)baz+", Some("foobarba")),
            ("a*b?", None),
            ("(ab)+c", Some("ab")),
            ("x|y", None),
            ("^static$", Some("static")),
            ("ab(cd|ef)ghi", Some("ghi")),
        ] {
            let (node, _) = parse(pat.as_bytes(), Options::default()).unwrap();
            let found = required(&node);
            assert_eq!(found.as_deref(), literal.map(str::as_bytes), "{pat}");
            assert!(!alone(&node), "{pat}");
        }
        for (pat, fixed, led) in [
            ("foo(bar)baz+", "foobarba", true),
            ("abc(d|e)", "abc", true),
            ("ab(cd|ef)ghi", "ghi", false),
            ("x*abc", "abc", false),
            ("(ab)+c", "ab", false),
        ] {
            let (node, _) = parse(pat.as_bytes(), Options::default()).unwrap();
            assert_eq!(leads(&node, fixed.as_bytes()), led, "{pat}");
        }
        for pat in ["a", "static int", "a\\.b"] {
            let (node, _) = parse(pat.as_bytes(), Options::default()).unwrap();
            assert!(alone(&node), "{pat}");
        }
    }

    #[test]
    fn fixed_bytes_are_found_where_they_first_lie() {
        for (fixed, text, at) in [
            ("a", "bca", Some(2)),
            ("ab", "aab", Some(1)),
            ("abc", "ababc", Some(2)),
            ("abc", "abab", None),
            ("aab", "aaab", Some(1)),
            ("abcabd", "abcabcabd", Some(3)),
        ] {
            let fixed = Fixed::new(fixed.as_bytes().to_vec());
            assert_eq!(fixed.find(text.as_bytes()), at, "{text}");
            assert_eq!(fixed.lies(text.as_bytes(), 0).next(), at, "{text}");
        }
        let twice = Fixed::new(b"aa".to_vec());
        assert_eq!(twice.lies(b"aaaa", 0).collect::<Vec<_>>(), [0, 1, 2]);
        assert_eq!(twice.lies(b"aaaa", 2).collect::<Vec<_>>(), [2]);
    }

    #[test]
    fn a_text_that_begins_the_bytes_everywhere_is_read_once() {
        // each place of the text begins the bytes and none ends them:
        // comparing the rest at every place would compare each byte as
        // often as the bytes are long
        let fixed = Fixed::new([vec![b'a'; 3_000], vec![b'b']].concat());
        let mut text = vec![b'a'; 30_000];
        let mut compared = 0;
        assert_eq!(fixed.sought(&text, &mut compared), None);
        assert!(compared <= 16 * text.len() + 3_064, "{compared}");
        text.push(b'b');
        compared = 0;
        assert_eq!(fixed.sought(&text, &mut compared), Some(27_000));
        assert!(compared <= 16 * text.len() + 3_064, "{compared}");
        // where the rest is seldom compared in vain nothing is counted
        let fixed = Fixed::new(b"static int".to_vec());
        compared = 0;
        assert_eq!(fixed.sought(b"some static int", &mut compared), Some(5));
        assert_eq!(compared, 0);
    }

    #[test]
    fn what_a_program_begins_with_is_a_run_of_bytes_that_count_alike() {
        let lead = |pattern: &str, ignore_case: bool| {
            let options = Options {
                ignore_case,
                ..Options::default()
            };
            let (node, groups, shape) =
                crate::syntax::parse_shaped(pattern.as_bytes(), options).unwrap();
            let prog = crate::program::compile(&node, groups, options, shape).unwrap();
            begins(&prog).map(|(lead, behind)| (lead.to_vec(), prog.insts[behind as usize]))
        };
        assert_eq!(lead("ab", false), Some((vec![0, 1], Inst::Match)));
        assert_eq!(lead("(aba)[xy]c*", true).unwrap().0, [0, 1, 0, 2]);
        // a set that lies across an earlier one ends the run
        assert_eq!(lead("a[ab]", false).unwrap().0, [0]);
        assert_eq!(lead("[ab]a", false).unwrap().0, [0]);
        assert_eq!(lead("x*a", false), None);
        let (node, groups, shape) =
            crate::syntax::parse_shaped(b"aBa[xy]", Options::default()).unwrap();
        let prog = crate::program::compile(&node, groups, Options::default(), shape).unwrap();
        let (found, _) = begins(&prog).unwrap();
        assert_eq!(found.lies(b"aBaBaBaxaBay", 0).collect::<Vec<_>>(), [4, 8]);
    }
}
