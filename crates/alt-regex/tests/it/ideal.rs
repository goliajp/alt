//! The match POSIX defines, found by trying everything, against the one
//! this crate finds: for the patterns the C library does not hold to
//! the meaning of their operators, so that it cannot be the reference.
//! Those are an anchor or the edge of a word inside or behind something
//! repeated, and a repetition without bound of what may match nothing.
//! Texts of bytes and the extended syntax only.

use std::collections::BTreeSet;

use alt_regex::{Options, Regex};

pub(crate) mod groups;
pub(crate) mod judged;
mod written;

pub(crate) use written::{Rng, Writer, text};

pub(crate) enum Node {
    /// A parenthesized subexpression and its number, from 1.
    Group(Box<Node>, usize),
    Set(Box<[bool; 256]>),
    Look(u8),
    Concat(Vec<Node>),
    Alt(Vec<Node>),
    Repeat(Box<Node>, u32, Option<u32>),
}

/// Reads the patterns [`Writer`] writes, and nothing else.
struct Reader<'a> {
    pattern: &'a [u8],
    at: usize,
    ignore_case: bool,
    groups: usize,
}

fn word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn set(holds: impl Fn(u8) -> bool) -> Node {
    let mut set = Box::new([false; 256]);
    (0..=255u8).for_each(|b| set[usize::from(b)] = holds(b));
    Node::Set(set)
}

impl Reader<'_> {
    fn peek(&self) -> Option<u8> {
        self.pattern.get(self.at).copied()
    }

    fn next(&mut self) -> u8 {
        self.at += 1;
        self.pattern[self.at - 1]
    }

    fn alternation(&mut self) -> Node {
        let mut branches = vec![self.concat()];
        while self.peek() == Some(b'|') {
            self.at += 1;
            branches.push(self.concat());
        }
        Node::Alt(branches)
    }

    fn concat(&mut self) -> Node {
        let mut items = Vec::new();
        while self.peek().is_some_and(|b| b != b'|' && b != b')') {
            let atom = self.atom();
            items.push(self.repeats(atom));
        }
        Node::Concat(items)
    }

    fn number(&mut self) -> Option<u32> {
        let from = self.at;
        while self.peek().is_some_and(|b| b.is_ascii_digit()) {
            self.at += 1;
        }
        let digits = std::str::from_utf8(&self.pattern[from..self.at]).unwrap();
        digits.parse().ok()
    }

    fn repeats(&mut self, mut atom: Node) -> Node {
        loop {
            let (min, max) = match self.peek() {
                Some(b'*') => (0, None),
                Some(b'+') => (1, None),
                Some(b'?') => (0, Some(1)),
                Some(b'{') => {
                    self.at += 1;
                    let min = self.number();
                    match self.peek() {
                        Some(b',') => {
                            self.at += 1;
                            (min.unwrap_or(0), self.number())
                        }
                        _ => (min.unwrap(), min),
                    }
                }
                _ => return atom,
            };
            self.at += 1;
            atom = Node::Repeat(Box::new(atom), min, max);
        }
    }

    fn literal(&self, b: u8) -> Node {
        let ignore_case = self.ignore_case;
        set(|other| other == b || (ignore_case && other.eq_ignore_ascii_case(&b)))
    }

    fn atom(&mut self) -> Node {
        match self.next() {
            b'(' => {
                self.groups += 1;
                let number = self.groups;
                let inner = self.alternation();
                assert_eq!(self.next(), b')');
                Node::Group(Box::new(inner), number)
            }
            look @ (b'^' | b'$') => Node::Look(look),
            b'.' => set(|b| b != 0),
            b'\\' => match self.next() {
                look @ (b'b' | b'B' | b'<' | b'>') => Node::Look(look),
                class @ (b'w' | b'W') => set(|b| word(b) == (class == b'w')),
                b => self.literal(b),
            },
            b'[' => self.bracket(),
            b => self.literal(b),
        }
    }

    fn bracket(&mut self) -> Node {
        let negated = self.peek() == Some(b'^');
        self.at += usize::from(negated);
        let mut members = [false; 256];
        let mut first = true;
        while first || self.peek() != Some(b']') {
            first = false;
            let rest = &self.pattern[self.at..];
            if rest.starts_with(b"[:alpha:]") {
                (0..=255u8).for_each(|b| members[usize::from(b)] |= b.is_ascii_alphabetic());
                self.at += 9;
            } else if rest.starts_with(b"[:space:]") {
                (0..=255u8)
                    .for_each(|b| members[usize::from(b)] |= b" \t\n\r\x0b\x0c".contains(&b));
                self.at += 9;
            } else if rest[1] == b'-' && rest[2] != b']' {
                (rest[0]..=rest[2]).for_each(|b| members[usize::from(b)] = true);
                self.at += 3;
            } else {
                members[usize::from(rest[0])] = true;
                self.at += 1;
            }
        }
        self.at += 1;
        let ignore_case = self.ignore_case;
        set(|b| {
            let other = match b.is_ascii_lowercase() {
                true => b.to_ascii_uppercase(),
                false => b.to_ascii_lowercase(),
            };
            let held = members[usize::from(b)] || (ignore_case && members[usize::from(other)]);
            held != negated
        })
    }
}

pub(crate) fn read(pattern: &[u8], ignore_case: bool) -> Node {
    let mut reader = Reader {
        pattern,
        at: 0,
        ignore_case,
        groups: 0,
    };
    let node = reader.alternation();
    assert_eq!(reader.at, pattern.len(), "{}", pattern.escape_ascii());
    node
}

fn holds(look: u8, text: &[u8], at: usize) -> bool {
    let before = at > 0 && word(text[at - 1]);
    let after = at < text.len() && word(text[at]);
    match look {
        b'^' => at == 0,
        b'$' => at == text.len(),
        b'b' => before != after,
        b'B' => before == after,
        b'<' => !before && after,
        _ => before && !after,
    }
}

/// Every position a match of `node` that begins at one of `from` ends at.
fn step(node: &Node, text: &[u8], from: &BTreeSet<usize>) -> BTreeSet<usize> {
    from.iter().flat_map(|&at| ends(node, text, at)).collect()
}

/// Every position a match of `node` that begins at `at` ends at.
fn ends(node: &Node, text: &[u8], at: usize) -> BTreeSet<usize> {
    match node {
        Node::Group(inner, _) => ends(inner, text, at),
        Node::Set(set) => {
            let taken = at < text.len() && set[usize::from(text[at])];
            taken.then_some(at + 1).into_iter().collect()
        }
        Node::Look(look) => holds(*look, text, at).then_some(at).into_iter().collect(),
        Node::Concat(items) => {
            let start = BTreeSet::from([at]);
            items.iter().fold(start, |now, item| step(item, text, &now))
        }
        Node::Alt(branches) => branches.iter().flat_map(|b| ends(b, text, at)).collect(),
        Node::Repeat(inner, min, max) => {
            let mut now = BTreeSet::from([at]);
            for _ in 0..*min {
                now = step(inner, text, &now);
            }
            let mut all = now.clone();
            let mut turns = *min;
            while max.is_none_or(|max| turns < max) {
                now = step(inner, text, &now);
                turns += 1;
                let before = all.len();
                all.extend(now.iter().copied());
                // without a bound, a turn that ends nowhere new is the last
                if max.is_none() && all.len() == before {
                    break;
                }
            }
            all
        }
    }
}

/// The earliest match and of those the longest.
fn ideal(node: &Node, text: &[u8]) -> Option<(usize, usize)> {
    (0..=text.len()).find_map(|from| ends(node, text, from).last().map(|&to| (from, to)))
}

/// Compares `count` random patterns; returns how many texts matched.
fn compare(seed: u64, count: usize, ignore_case: bool) -> usize {
    let mut rng = Rng(seed);
    let options = Options {
        ignore_case,
        ..Options::default()
    };
    let mut matched = 0;
    for _ in 0..count {
        let mut writer = Writer {
            rng: &mut rng,
            edges: true,
            out: Vec::new(),
        };
        writer.expression(2);
        let pattern = writer.out;
        let ours = Regex::with_options(&pattern, options).unwrap();
        let node = read(&pattern, ignore_case);
        for _ in 0..6 {
            let text = text(&mut rng);
            let wanted = ideal(&node, &text);
            let shown = || format!("{} on {}", pattern.escape_ascii(), text.escape_ascii());
            let found = ours.captures(&text).and_then(|found| found.get(0));
            assert_eq!(
                found.clone().map(|r| (r.start, r.end)),
                wanted,
                "{}",
                shown()
            );
            assert_eq!(ours.find(&text), found, "{}", shown());
            assert_eq!(ours.is_match(&text), wanted.is_some(), "{}", shown());
            matched += usize::from(wanted.is_some());
        }
    }
    matched
}

#[test]
fn edges_and_anchors_around_repetitions_hold_where_posix_puts_them() {
    // each of these the C library answers otherwise
    for (pattern, text, wanted) in [
        ("a*\\B", "xa", Some((1, 1))),
        ("a{0,1}\\B", "xa", Some((1, 1))),
        ("(a|)\\B", "xa", Some((1, 1))),
        ("(\\ba){2,}", "aaa", None),
        ("(^a?){1,2}", "a", Some((0, 1))),
        ("(^a){,2}ab?", "bxaab", Some((2, 3))),
        ("b(^a{2}){,2}", "baaa", Some((0, 1))),
        ("|(\\>a?){,2}", "x", Some((0, 0))),
        ("($a?){,2}|", "ab", Some((0, 0))),
        ("($a+){,2}\\B", "aa", Some((1, 1))),
        ("(\\Ba?|)+", "a  x", Some((0, 0))),
    ] {
        let node = read(pattern.as_bytes(), false);
        assert_eq!(ideal(&node, text.as_bytes()), wanted, "{pattern}");
        let found = Regex::new(pattern.as_bytes())
            .unwrap()
            .find(text.as_bytes());
        assert_eq!(found.map(|r| (r.start, r.end)), wanted, "{pattern}");
    }
}

#[test]
fn random_expressions_match_where_posix_defines_the_match() {
    assert!(compare(0x9e3779b97f4a7c15, 6_000, false) > 12_000);
    assert!(compare(0xd1342543de82ef95, 3_000, true) > 6_000);
}

#[test]
#[ignore = "two million random patterns against a search of every path, needs --release"]
fn random_expressions_match_where_posix_defines_the_match_long() {
    assert!(compare(0x2545f4914f6cdd1d, 1_500_000, false) > 3_000_000);
    assert!(compare(0x94d049bb133111eb, 500_000, true) > 1_000_000);
}
