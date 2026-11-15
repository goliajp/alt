//! Sets of characters as the byte sequences of their UTF-8 forms, so
//! that the matchers go on stepping bytes.

use crate::chars::{self, CharSet};
use crate::class::ByteSet;
use crate::syntax::Node;

fn span(from: u8, to: u8) -> Node {
    let mut set = ByteSet::empty();
    set.insert_range(from, to);
    Node::Set(set)
}

fn one_of(mut branches: Vec<Node>) -> Node {
    match branches.len() {
        // a set with nothing in it matches nothing
        0 => Node::Set(ByteSet::empty()),
        1 => branches.pop().expect("one branch"),
        _ => Node::Alt(branches),
    }
}

/// `first` and behind it `rest`, in one sequence.
fn then(first: Node, rest: Node) -> Node {
    match rest {
        Node::Concat(mut items) => {
            items.insert(0, first);
            Node::Concat(items)
        }
        rest => Node::Concat(vec![first, rest]),
    }
}

/// `count` bytes that go on a character, each any of them.
fn tails(count: u32) -> Node {
    let each = span(0x80, 0xbf);
    match count {
        1 => each,
        _ => Node::Concat(vec![each; count as usize]),
    }
}

/// How much of `from..=to` the ranges hold: none, some or all of it.
fn share(ranges: &[(u32, u32)], from: u32, to: u32) -> (bool, bool) {
    let at = ranges.partition_point(|&(_, high)| high < from);
    match ranges.get(at) {
        Some(&(low, high)) if low <= to => (true, low <= from && high >= to),
        _ => (false, false),
    }
}

/// The sequences of the characters of `ranges` in `from..=to`, which
/// share the bytes before the one `left` bytes from the last and take
/// `len` bytes in all.
fn sequences(ranges: &[(u32, u32)], from: u32, to: u32, left: u32, len: u32) -> Node {
    let shift = 6 * left;
    let byte = |c: u32| match left + 1 == len {
        true => ([0, 0, 0xc0, 0xe0, 0xf0, 0xf8, 0xfc][len as usize] | c >> shift) as u8,
        false => (0x80 | (c >> shift & 0x3f)) as u8,
    };
    // the bytes that lead to the same sequences stand together
    let mut groups: Vec<(ByteSet, Option<Node>)> = Vec::new();
    let mut start = from;
    while start <= to {
        let whole = start | ((1 << shift) - 1);
        let end = to.min(whole);
        let (some, all) = share(ranges, start, end);
        // only a byte with every sequence behind it has them all
        let all = all && end == whole && start & ((1 << shift) - 1) == 0;
        if some {
            let rest = match (left, all) {
                (0, _) => None,
                (_, true) => Some(tails(left)),
                (_, false) => Some(sequences(ranges, start, end, left - 1, len)),
            };
            match groups.iter_mut().find(|(_, known)| *known == rest) {
                Some((bytes, _)) => bytes.insert(byte(start)),
                None => groups.push((ByteSet::byte(byte(start)), rest)),
            }
        }
        start = end + 1;
    }
    let branch = |(bytes, rest): (ByteSet, Option<Node>)| match rest {
        Some(rest) => then(Node::Set(bytes), rest),
        None => Node::Set(bytes),
    };
    one_of(groups.into_iter().map(branch).collect())
}

/// What matches one character of `set`.
pub fn node(set: &CharSet) -> Node {
    let ranges = set.ranges();
    let mut branches = Vec::new();
    let mut ascii = ByteSet::empty();
    for &(from, to) in ranges.iter().take_while(|&&(from, _)| from < 0x80) {
        ascii.insert_range(from as u8, to.min(0x7f) as u8);
    }
    if !ascii.is_empty() {
        branches.push(Node::Set(ascii));
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
            match sequences(ranges, from, to, len - 1, len) {
                Node::Alt(more) => branches.extend(more),
                one => branches.push(one),
            }
        }
    }
    one_of(branches)
}

/// What matches the character or stray byte `rest` begins with, and how
/// many bytes of the pattern that is. Without regard to case a character
/// matches whatever reads as it in upper case; a stray byte only itself.
pub fn literal(rest: &[u8], ignore_case: bool) -> (Node, usize) {
    let Some((c, len)) = chars::decode(rest) else {
        return (Node::Set(ByteSet::byte(rest[0])), 1);
    };
    if ignore_case {
        return (node(&CharSet::alike(c)), len);
    }
    let bytes = rest[..len].iter().map(|&b| Node::Set(ByteSet::byte(b)));
    match len {
        1 => (Node::Set(ByteSet::byte(rest[0])), 1),
        _ => (Node::Concat(bytes.collect()), len),
    }
}

/// What `.` matches: any character but NUL and, with `newline`, the
/// newline. A pattern the C library can match byte by byte is `loose`:
/// there it takes any sequence shaped like a character, of up to six
/// bytes, also one that encodes no character.
pub fn period(newline: bool, loose: bool) -> Node {
    if !loose {
        let all = CharSet::new(vec![(1, u32::MAX)]);
        let gone: &[(u32, u32)] = if newline { &[(0x0a, 0x0a)] } else { &[] };
        return node(&all.without(gone));
    }
    let mut ascii = ByteSet::empty();
    ascii.insert_range(1, 0x7f);
    if newline {
        ascii.remove(b'\n');
    }
    let shaped = |lead: (u8, u8), second: u8, more: u32| {
        let mut items = vec![span(lead.0, lead.1), span(second, 0xbf)];
        items.extend((0..more).map(|_| span(0x80, 0xbf)));
        Node::Concat(items)
    };
    Node::Alt(vec![
        Node::Set(ascii),
        shaped((0xc2, 0xdf), 0x80, 0),
        shaped((0xe0, 0xe0), 0xa0, 1),
        shaped((0xe1, 0xef), 0x80, 1),
        shaped((0xf0, 0xf0), 0x90, 2),
        shaped((0xf1, 0xf7), 0x80, 2),
        shaped((0xf8, 0xf8), 0x88, 3),
        shaped((0xf9, 0xfb), 0x80, 3),
        shaped((0xfc, 0xfc), 0x84, 4),
        shaped((0xfd, 0xfd), 0x80, 4),
    ])
}

/// What `\w` and `\s` match, or with `negated` their opposites.
pub fn named(name: &[u8], negated: bool) -> Node {
    let mut ranges = chars::class(name, false).expect("a known class");
    if name == b"alnum" {
        ranges.push((u32::from(b'_'), u32::from(b'_')));
    }
    let set = CharSet::new(ranges);
    node(&match negated {
        true => set.negated(),
        false => set,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::program::compile;
    use crate::{Options, pike};

    /// Whether `node` matches all of `text`.
    fn takes(node: &Node, text: &[u8]) -> bool {
        let whole = Node::Concat(vec![node.clone(), Node::Look(crate::syntax::Look::TextEnd)]);
        let prog = compile(&whole, 0, Options::default(), Default::default()).unwrap();
        let mut slots = [pike::UNSET; 2];
        pike::search(&prog, &mut Default::default(), text, Some(0), &mut slots)
    }

    fn encoded(c: u32) -> Vec<u8> {
        let mut buf = [0; 4];
        char::from_u32(c)
            .unwrap()
            .encode_utf8(&mut buf)
            .as_bytes()
            .to_vec()
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
        let any = node(&CharSet::new(vec![(0, 0x10ffff)]));
        for c in [
            0, 0x7f, 0x80, 0x7ff, 0x800, 0xd7ff, 0xe000, 0xffff, 0x10000, 0x10ffff,
        ] {
            assert!(takes(&any, &encoded(c)), "{c:x}");
        }
        for bad in [
            &b"\xed\xa0\x80"[..],
            b"\xc0\x80",
            b"\xf4\x90\x80\x80",
            b"\xe0\x80\x80",
            b"\xff",
        ] {
            assert!(!takes(&any, bad), "{bad:?}");
        }
    }

    #[test]
    fn a_period_takes_one_character_and_a_loose_one_what_is_shaped_like_one() {
        let strict = period(true, false);
        assert!(takes(&strict, "é".as_bytes()) && takes(&strict, b"a"));
        assert!(!takes(&strict, b"\n") && !takes(&strict, b"\0") && !takes(&strict, b"\xe9"));
        assert!(!takes(&strict, b"\xed\xa0\x80"));
        let loose = period(false, true);
        assert!(takes(&loose, "€".as_bytes()) && takes(&loose, b"\n"));
        assert!(takes(&loose, b"\xed\xa0\x80") && takes(&loose, b"\xf8\x88\x80\x80\x80"));
        assert!(!takes(&loose, b"\xe9") && !takes(&loose, b"\xc0\x80") && !takes(&loose, b"\0"));
    }

    #[test]
    fn a_literal_without_regard_to_case_takes_what_reads_alike() {
        let (node, len) = literal("éa".as_bytes(), true);
        assert_eq!(len, 2);
        assert!(takes(&node, "É".as_bytes()) && takes(&node, "é".as_bytes()));
        assert!(!takes(&node, b"e"));
        let (node, _) = literal(b"\xe9", true);
        assert!(takes(&node, b"\xe9") && !takes(&node, b"\xc9"));
    }
}
