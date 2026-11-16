//! A set of characters as instructions over the bytes of their UTF-8
//! forms, so that the matchers go on stepping bytes. The instructions of
//! a set are made once and copied into each program that uses it; forms
//! that end alike share their ends.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::chars::CharSet;
use crate::class::ByteSet;
use crate::program::Inst;
use crate::syntax::Node;

/// The target of an instruction that leaves the block.
pub const EXIT: u32 = u32::MAX;

/// The instructions that take one character of a set. Targets count
/// from the start of the block, and sets from its own.
#[derive(Debug, PartialEq)]
pub struct Block {
    pub insts: Vec<Inst>,
    pub sets: Vec<ByteSet>,
    pub start: u32,
}

/// The bytes that lead on from one place in a form, each set with the
/// place it leads to.
type Edges = Vec<(ByteSet, u32)>;

/// A hash for the many small keys of one builder; nothing an outsider
/// chooses is hashed with it.
#[derive(Default)]
struct Mix(u64);

impl std::hash::Hasher for Mix {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(8) {
            let mut word = [0; 8];
            word[..chunk.len()].copy_from_slice(chunk);
            self.write_u64(u64::from_le_bytes(word));
        }
    }

    fn write_u64(&mut self, word: u64) {
        self.0 = (self.0.rotate_left(5) ^ word).wrapping_mul(0x517c_c1b7_2722_0a95);
    }

    fn write_u32(&mut self, word: u32) {
        self.write_u64(u64::from(word));
    }

    fn write_usize(&mut self, word: usize) {
        self.write_u64(word as u64);
    }
}

type Mixed = std::hash::BuildHasherDefault<Mix>;

#[derive(Default)]
struct Builder {
    places: Vec<Edges>,
    known: HashMap<Edges, u32, Mixed>,
}

/// How much of `from..=to` the ranges hold: some of it, and all of it.
fn share(ranges: &[(u32, u32)], from: u32, to: u32) -> (bool, bool) {
    let at = ranges.partition_point(|&(_, high)| high < from);
    match ranges.get(at) {
        Some(&(low, high)) if low <= to => (true, low <= from && high >= to),
        _ => (false, false),
    }
}

impl Builder {
    fn place(&mut self, edges: Edges) -> u32 {
        if let Some(&known) = self.known.get(&edges) {
            return known;
        }
        self.places.push(edges.clone());
        let place = self.places.len() as u32 - 1;
        self.known.insert(edges, place);
        place
    }

    /// The place before `count` bytes that go on a character, each any
    /// of them.
    fn tails(&mut self, count: u32) -> u32 {
        let mut all = ByteSet::empty();
        all.insert_range(0x80, 0xbf);
        (0..count).fold(EXIT, |behind, _| self.place(vec![(all, behind)]))
    }

    /// The bytes that begin the rest of the characters of `ranges` in
    /// `from..=to`, which share the bytes before; `left` more bytes
    /// follow this one, in a form of `len`.
    fn edges(&mut self, ranges: &[(u32, u32)], from: u32, to: u32, left: u32, len: u32) -> Edges {
        let shift = 6 * left;
        let byte = |c: u32| match left + 1 == len {
            true => ([0, 0, 0xc0, 0xe0, 0xf0, 0xf8, 0xfc][len as usize] | c >> shift) as u8,
            false => (0x80 | (c >> shift & 0x3f)) as u8,
        };
        if left == 0 {
            let mut last = ByteSet::empty();
            let first = ranges.partition_point(|&(_, high)| high < from);
            for &(low, high) in ranges[first..].iter().take_while(|&&(low, _)| low <= to) {
                last.insert_range(byte(low.max(from)), byte(high.min(to)));
            }
            return vec![(last, EXIT)];
        }
        let mut edges: Edges = Vec::new();
        let mut start = from;
        while start <= to {
            let whole = start | ((1 << shift) - 1);
            let end = to.min(whole);
            let (some, all) = share(ranges, start, end);
            if some {
                // only a byte with every sequence behind it has them all
                let all = all && end == whole && start & ((1 << shift) - 1) == 0;
                let behind = match all {
                    true => self.tails(left),
                    false => {
                        let rest = self.edges(ranges, start, end, left - 1, len);
                        self.place(rest)
                    }
                };
                match edges.iter_mut().find(|(_, known)| *known == behind) {
                    Some((bytes, _)) => bytes.insert(byte(start)),
                    None => edges.push((ByteSet::byte(byte(start)), behind)),
                }
            }
            start = end + 1;
        }
        edges
    }

    /// The places as instructions, each after the ones it leads to.
    fn block(self, start: u32) -> Block {
        let mut insts = Vec::new();
        let mut sets: Vec<ByteSet> = Vec::new();
        let mut numbers: HashMap<ByteSet, u32, Mixed> = HashMap::default();
        let mut entries = Vec::with_capacity(self.places.len());
        for edges in &self.places {
            let first = insts.len() as u32;
            for (bytes, behind) in edges {
                let target = match *behind {
                    EXIT => EXIT,
                    place => entries[place as usize],
                };
                insts.push(match bytes.single() {
                    Some(b) => Inst::Byte(b, target),
                    None => {
                        let number = *numbers.entry(*bytes).or_insert_with(|| {
                            sets.push(*bytes);
                            sets.len() as u32 - 1
                        });
                        Inst::Set(number, target)
                    }
                });
            }
            // the choice between them, the last two first
            let mut entry = insts.len() as u32 - 1;
            for edge in (first..entry).rev() {
                insts.push(Inst::Split(edge, entry));
                entry = insts.len() as u32 - 1;
            }
            entries.push(entry);
        }
        Block {
            insts,
            sets,
            start: entries[start as usize],
        }
    }
}

/// What matches one character of `set`.
pub fn node(set: &CharSet) -> Node {
    let ranges = set.ranges();
    let mut ascii = ByteSet::empty();
    for &(from, to) in ranges.iter().take_while(|&&(from, _)| from < 0x80) {
        ascii.insert_range(from as u8, to.min(0x7f) as u8);
    }
    if ranges.last().is_none_or(|&(_, to)| to < 0x80) {
        // a set with nothing in it matches nothing
        return Node::Set(ascii);
    }
    let mut builder = Builder::default();
    let mut edges = Vec::new();
    if !ascii.is_empty() {
        edges.push((ascii, EXIT));
    }
    let forms = [
        (2, 0x80, 0x7ff),
        (3, 0x800, 0xffff),
        (4, 0x1_0000, 0x1f_ffff),
        (5, 0x20_0000, 0x3ff_ffff),
        (6, 0x400_0000, 0x7fff_ffff),
    ];
    for (len, from, to) in forms {
        if share(ranges, from, to).0 {
            edges.extend(builder.edges(ranges, from, to, len - 1, len));
        }
    }
    let start = builder.place(edges);
    Node::Class(Arc::new(builder.block(start)))
}

/// The sets made so far, by what was written and how it was read.
type Known = HashMap<(Vec<u8>, u8), Node>;

/// The most sets remembered.
const REMEMBERED: usize = 512;

/// What matches one character of the set that `written` and `flags`
/// name; `set` makes it the first time. The same few sets are written
/// again and again, in one pattern and in the next.
pub fn remembered(written: &[u8], flags: u8, set: impl FnOnce() -> CharSet) -> Node {
    static KNOWN: Mutex<Option<Known>> = Mutex::new(None);
    let key = (written.to_vec(), flags);
    let known = |map: &mut Option<Known>| map.get_or_insert_default().get(&key).cloned();
    if let Some(node) = known(&mut KNOWN.lock().expect("no panic under the lock")) {
        return node;
    }
    let made = node(&set());
    let mut map = KNOWN.lock().expect("no panic under the lock");
    let map = map.get_or_insert_default();
    if map.len() < REMEMBERED {
        map.insert(key, made.clone());
    }
    made
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::program::compile;
    use crate::{Options, pike};

    /// Whether `node` matches all of `text`.
    pub fn takes(node: &Node, text: &[u8]) -> bool {
        let end = Node::Look(crate::syntax::Look::TextEnd);
        let whole = Node::Concat(vec![node.clone(), end]);
        let prog = compile(&whole, 0, Options::default(), Default::default()).unwrap();
        let mut slots = [pike::UNSET; 2];
        pike::search(&prog, &mut Default::default(), text, Some(0), &mut slots)
    }

    fn encoded(c: u32) -> Vec<u8> {
        char::from_u32(c).unwrap().to_string().into_bytes()
    }

    #[test]
    fn a_set_takes_the_sequences_of_its_characters_and_no_others() {
        let ranges = vec![
            (0x61, 0x7a),
            (0xe0, 0xff),
            (0x7fe, 0x801),
            (0xfffd, 0x10001),
            (0x10fff0, 0x10ffff),
        ];
        let set = CharSet::new(ranges);
        let node = node(&set);
        let edges = set.ranges().iter().flat_map(|&(from, to)| [from, to]);
        for edge in edges.collect::<Vec<_>>() {
            for c in edge.saturating_sub(2)..=(edge + 2).min(0x10ffff) {
                if char::from_u32(c).is_some() {
                    assert_eq!(takes(&node, &encoded(c)), set.contains(c), "{c:x}");
                }
            }
        }
        assert!(!takes(&node, b"\xc3") && !takes(&node, b"\xa9") && !takes(&node, b"ab"));
    }

    #[test]
    fn every_character_but_the_surrogates_has_its_sequence() {
        let any = node(&CharSet::new(vec![(0, u32::MAX)]));
        for c in [
            0, 0x7f, 0x80, 0x7ff, 0x800, 0xd7ff, 0xe000, 0xffff, 0x10000, 0x10ffff,
        ] {
            assert!(takes(&any, &encoded(c)), "{c:x}");
        }
        // past Unicode, as the C library reads them
        assert!(takes(&any, b"\xf4\x90\x80\x80") && takes(&any, b"\xfd\xbf\xbf\xbf\xbf\xbf"));
        for bad in [
            &b"\xed\xa0\x80"[..],
            b"\xc0\x80",
            b"\xf0\x8f\xbf\xbf",
            b"\xe0\x80\x80",
            b"\xf8\x87\xbf\xbf\xbf",
            b"\xff",
        ] {
            assert!(!takes(&any, bad), "{bad:?}");
        }
    }

    #[test]
    fn forms_that_end_alike_share_their_ends() {
        let Node::Class(block) = node(&CharSet::new(vec![(0, u32::MAX)])) else {
            panic!("a set beyond ASCII is a block");
        };
        // one instruction for each of the five bytes that may follow,
        // and the leading bytes that differ in what follows them
        assert!(block.insts.len() < 40, "{}", block.insts.len());
        let twice = remembered(b"[x]", 0, || CharSet::new(vec![(0x100, 0x200)]));
        let again = remembered(b"[x]", 0, || unreachable!("made once"));
        assert_eq!(twice, again);
    }
}
