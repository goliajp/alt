//! Parsing POSIX basic and extended regular expressions, with the GNU
//! operators (`\w`, `\s`, `\b`, `\<` and their kin) that the C library
//! accepts in both.

use crate::class::ByteSet;
use crate::{Error, Options, Syntax, bracket, utf8};

/// A condition on a position that consumes nothing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Look {
    TextStart,
    TextEnd,
    LineStart,
    LineEnd,
    WordBoundary,
    NotWordBoundary,
    WordStart,
    WordEnd,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    Empty,
    Set(ByteSet),
    Look(Look),
    /// A parenthesized subexpression and its number, from 1.
    Group(Box<Node>, u32),
    Concat(Vec<Node>),
    Alt(Vec<Node>),
    /// What the group of this number matched, once more.
    BackRef(u32),
    /// At least `.1` and at most `.2` repetitions.
    Repeat(Box<Node>, u32, Option<u32>),
}

/// The largest repetition count, as in the C library.
const DUP_MAX: u32 = 0x7fff;
/// The deepest nesting of groups and of repetitions around one atom.
const MAX_DEPTH: u32 = 200;
/// The most groups a pattern may have.
pub const MAX_GROUPS: u32 = 0x7fff;
/// The longest pattern read, in bytes.
pub const MAX_PATTERN: usize = 1 << 18;

struct Parser<'a> {
    pat: &'a [u8],
    at: usize,
    opts: Options,
    groups: u32,
    depth: u32,
    /// the groups whose closing parenthesis has been read
    complete: Vec<u32>,
    /// whether a period was read, and whether something the C library
    /// cannot match byte by byte in a UTF-8 text
    period: bool,
    complex: bool,
    strays: bool,
    /// see [`utf8::period`]
    loose: bool,
}

/// What the matchers need to know of a pattern besides its tree.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Shape {
    /// The C library reads the pattern character by character, so a
    /// match begins only where a character does. Otherwise it matches
    /// bytes, also a UTF-8 text.
    pub apart: bool,
    /// A byte written in the pattern goes on a character and begins
    /// none.
    pub strays: bool,
}

/// Parses `pat` into its tree and the number of groups in it.
#[cfg(test)]
pub fn parse(pat: &[u8], opts: Options) -> Result<(Node, u32), Error> {
    parse_shaped(pat, opts).map(|(node, groups, _)| (node, groups))
}

/// As [`parse`], with the shape of the pattern.
pub fn parse_shaped(pat: &[u8], opts: Options) -> Result<(Node, u32, Shape), Error> {
    if pat.len() > MAX_PATTERN {
        return Err(Error::TooBig);
    }
    let read = |loose: bool| {
        let mut p = Parser {
            pat,
            at: 0,
            opts,
            groups: 0,
            depth: 0,
            complete: Vec::new(),
            period: false,
            complex: opts.ignore_case,
            strays: false,
            loose,
        };
        let node = p.alternation()?;
        if p.at < pat.len() {
            return Err(Error::Paren);
        }
        let shape = Shape {
            apart: opts.utf8 && p.complex,
            strays: p.strays,
        };
        Ok((node, p.groups, shape, p.period))
    };
    let (node, groups, shape, period) = read(false)?;
    if opts.utf8 && period && !shape.apart {
        return read(true).map(|(node, groups, shape, _)| (node, groups, shape));
    }
    Ok((node, groups, shape))
}

impl<'a> Parser<'a> {
    fn ere(&self) -> bool {
        self.opts.syntax == Syntax::Extended
    }

    fn peek(&self) -> Option<u8> {
        self.pat.get(self.at).copied()
    }

    fn rest(&self) -> &'a [u8] {
        &self.pat[self.at..]
    }

    fn at_alt(&self) -> bool {
        match self.ere() {
            true => self.peek() == Some(b'|'),
            false => self.rest().starts_with(b"\\|"),
        }
    }

    fn at_close(&self) -> bool {
        self.depth > 0
            && match self.ere() {
                true => self.peek() == Some(b')'),
                false => self.rest().starts_with(b"\\)"),
            }
    }

    fn alternation(&mut self) -> Result<Node, Error> {
        let before = self.complete.len();
        let mut branches = vec![self.concat()?];
        while self.at_alt() {
            self.at += if self.ere() { 1 } else { 2 };
            // as in the C library, a branch cannot refer to a group of
            // a branch beside it
            let beside = self.complete.split_off(before);
            branches.push(self.concat()?);
            self.complete.extend(beside);
        }
        Ok(match branches.len() {
            1 => branches.pop().expect("one branch"),
            _ => Node::Alt(branches),
        })
    }

    fn concat(&mut self) -> Result<Node, Error> {
        let mut items = Vec::new();
        while self.at < self.pat.len() && !self.at_alt() && !self.at_close() {
            // nothing to repeat yet: at the start, or behind an anchor
            let fresh = matches!(items.last(), None | Some(Node::Look(_)));
            let atom = self.atom(fresh)?;
            items.push(match atom {
                Node::Look(_) => atom,
                _ => self.repeats(atom)?,
            });
        }
        Ok(match items.len() {
            0 => Node::Empty,
            1 => items.pop().expect("one item"),
            _ => Node::Concat(items),
        })
    }

    fn repeats(&mut self, mut atom: Node) -> Result<Node, Error> {
        let mut nested = 0;
        loop {
            let rest = self.rest();
            let (skip, bounds) = match (self.ere(), rest) {
                (_, [b'*', ..]) => (1, Some((0, None))),
                (true, [b'+', ..]) => (1, Some((1, None))),
                (true, [b'?', ..]) => (1, Some((0, Some(1)))),
                (true, [b'{', ..]) => (1, None),
                (false, [b'\\', b'+', ..]) => (2, Some((1, None))),
                (false, [b'\\', b'?', ..]) => (2, Some((0, Some(1)))),
                (false, [b'\\', b'{', ..]) => (2, None),
                _ => return Ok(atom),
            };
            self.at += skip;
            let (min, max) = match bounds {
                Some(bounds) => bounds,
                None => self.interval()?,
            };
            atom = match atom {
                // no repetition of nothing is more than nothing
                Node::Empty => Node::Empty,
                _ if max == Some(0) => Node::Empty,
                _ if (min, max) == (1, Some(1)) => atom,
                // `*`, `+` and `?` behind one another are one of them
                Node::Repeat(inner, low @ 0..=1, high @ (None | Some(1)))
                    if min <= 1 && matches!(max, None | Some(1)) =>
                {
                    let once = high.is_some() && max.is_some();
                    Node::Repeat(inner, low * min, once.then_some(1))
                }
                _ => {
                    nested += 1;
                    if self.depth + nested > MAX_DEPTH {
                        return Err(Error::TooBig);
                    }
                    Node::Repeat(Box::new(atom), min, max)
                }
            };
        }
    }

    fn number(&mut self) -> Result<Option<u32>, Error> {
        let mut value = None;
        while let Some(digit @ b'0'..=b'9') = self.peek() {
            let next = value.unwrap_or(0) * 10 + u32::from(digit - b'0');
            if next > DUP_MAX {
                return Err(Error::TooBig);
            }
            value = Some(next);
            self.at += 1;
        }
        Ok(value)
    }

    /// The bounds of `{m}`, `{m,}`, `{m,n}` or `{,n}`, behind the brace.
    fn interval(&mut self) -> Result<(u32, Option<u32>), Error> {
        let min = self.number()?;
        let (min, max) = match (min, self.peek()) {
            (_, Some(b',')) => {
                self.at += 1;
                (min.unwrap_or(0), self.number()?)
            }
            (Some(min), _) => (min, Some(min)),
            (None, _) => return Err(Error::Brace),
        };
        let close: &[u8] = if self.ere() { b"}" } else { b"\\}" };
        if !self.rest().starts_with(close) {
            return Err(Error::Brace);
        }
        self.at += close.len();
        if max.is_some_and(|max| max < min) {
            return Err(Error::Brace);
        }
        Ok((min, max))
    }

    /// What the literal that the byte just read begins matches.
    fn literal(&mut self, b: u8) -> Node {
        if self.opts.utf8 {
            let (node, len) = utf8::literal(&self.pat[self.at - 1..], self.opts.ignore_case);
            self.at += len - 1;
            self.strays |= (0x80..=0xbf).contains(&b);
            return node;
        }
        let mut set = ByteSet::byte(b);
        if self.opts.ignore_case {
            set.fold_case();
        }
        Node::Set(set)
    }

    fn start(&self) -> Node {
        Node::Look(match self.opts.newline {
            true => Look::LineStart,
            false => Look::TextStart,
        })
    }

    fn end(&self) -> Node {
        Node::Look(match self.opts.newline {
            true => Look::LineEnd,
            false => Look::TextEnd,
        })
    }

    fn any(&mut self) -> Node {
        if self.opts.utf8 {
            self.period = true;
            return utf8::period(self.opts.newline, self.loose);
        }
        let mut set = ByteSet::full();
        set.remove(0);
        if self.opts.newline {
            set.remove(b'\n');
        }
        Node::Set(set)
    }

    fn atom(&mut self, fresh: bool) -> Result<Node, Error> {
        let b = self.pat[self.at];
        if b == b'\\' {
            return self.escape();
        }
        self.at += 1;
        if self.ere() {
            return match b {
                b'(' => self.group(),
                b'*' | b'+' | b'?' | b'{' => Err(Error::Repeat),
                b'^' => Ok(self.start()),
                b'$' => Ok(self.end()),
                b'.' => Ok(self.any()),
                b'[' => self.bracket(),
                _ => Ok(self.literal(b)),
            };
        }
        // in a basic expression an anchor is one only at the edge of a branch
        let last = matches!(self.rest(), [] | [b'\\', b')' | b'|', ..]);
        match b {
            b'^' if fresh => Ok(self.start()),
            b'$' if last => Ok(self.end()),
            b'.' => Ok(self.any()),
            b'[' => self.bracket(),
            _ => Ok(self.literal(b)),
        }
    }

    fn escape(&mut self) -> Result<Node, Error> {
        let Some(&c) = self.pat.get(self.at + 1) else {
            return Err(Error::Escape);
        };
        self.at += 2;
        self.complex |= matches!(c, b'w' | b'W' | b's' | b'S' | b'b' | b'B' | b'<' | b'>');
        let utf8 = self.opts.utf8;
        let named = |name: &[u8], negated: bool| {
            if utf8 {
                return utf8::named(name, negated);
            }
            let mut set = match name {
                b"alnum" => ByteSet::word(),
                _ => ByteSet::class(name).expect("a known class"),
            };
            if negated {
                set.negate();
            }
            Node::Set(set)
        };
        Ok(match c {
            b'1'..=b'9' => {
                let number = u32::from(c - b'0');
                if !self.complete.contains(&number) {
                    return Err(Error::BackReference);
                }
                Node::BackRef(number)
            }
            b'w' | b'W' => named(b"alnum", c == b'W'),
            b's' | b'S' => named(b"space", c == b'S'),
            b'b' => Node::Look(Look::WordBoundary),
            b'B' => Node::Look(Look::NotWordBoundary),
            b'<' => Node::Look(Look::WordStart),
            b'>' => Node::Look(Look::WordEnd),
            b'`' => Node::Look(Look::TextStart),
            b'\'' => Node::Look(Look::TextEnd),
            b'(' if !self.ere() => return self.group(),
            b')' if !self.ere() => return Err(Error::Paren),
            b'{' if !self.ere() => return Err(Error::Repeat),
            _ => self.literal(c),
        })
    }

    /// A group, behind its opening parenthesis.
    fn bracket(&mut self) -> Result<Node, Error> {
        let (node, complex) = bracket::parse(self.pat, &mut self.at, self.opts)?;
        self.complex |= complex;
        Ok(node)
    }

    fn group(&mut self) -> Result<Node, Error> {
        self.groups += 1;
        if self.groups > MAX_GROUPS {
            return Err(Error::TooBig);
        }
        let number = self.groups;
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(Error::TooBig);
        }
        let inner = self.alternation()?;
        if !self.at_close() {
            return Err(Error::Paren);
        }
        self.at += if self.ere() { 1 } else { 2 };
        self.depth -= 1;
        self.complete.push(number);
        Ok(Node::Group(Box::new(inner), number))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ere(pat: &str) -> Result<(Node, u32), Error> {
        parse(pat.as_bytes(), Options::default())
    }

    fn bre(pat: &str) -> Result<(Node, u32), Error> {
        let opts = Options {
            syntax: Syntax::Basic,
            ..Options::default()
        };
        parse(pat.as_bytes(), opts)
    }

    fn lit(b: u8) -> Node {
        Node::Set(ByteSet::byte(b))
    }

    #[test]
    fn groups_are_numbered_by_their_opening_parenthesis() {
        let (node, groups) = ere("(a(b))|(c)").unwrap();
        assert_eq!(groups, 3);
        let inner = Node::Group(Box::new(lit(b'b')), 2);
        let first = Node::Group(Box::new(Node::Concat(vec![lit(b'a'), inner])), 1);
        let third = Node::Group(Box::new(lit(b'c')), 3);
        assert_eq!(node, Node::Alt(vec![first, third]));
    }

    #[test]
    fn intervals_take_every_form_and_refuse_the_malformed() {
        let repeat = |min, max| Node::Repeat(Box::new(lit(b'a')), min, max);
        assert_eq!(ere("a{2}").unwrap().0, repeat(2, Some(2)));
        assert_eq!(ere("a{2,}").unwrap().0, repeat(2, None));
        assert_eq!(ere("a{2,5}").unwrap().0, repeat(2, Some(5)));
        assert_eq!(ere("a{,5}").unwrap().0, repeat(0, Some(5)));
        assert_eq!(bre("a\\{2,5\\}").unwrap().0, repeat(2, Some(5)));
        assert_eq!(ere("a{5,2}"), Err(Error::Brace));
        assert_eq!(ere("a{x}"), Err(Error::Brace));
        assert_eq!(ere("a{2"), Err(Error::Brace));
        assert_eq!(ere("a{40000}"), Err(Error::TooBig));
    }

    #[test]
    fn a_repetition_needs_something_to_repeat_in_an_extended_expression() {
        for pat in ["*a", "a|*b", "(+a)", "^*", "{2}"] {
            assert_eq!(ere(pat), Err(Error::Repeat), "{pat}");
        }
        // in a basic expression such a star stands for itself
        let star = Node::Repeat(Box::new(lit(b'*')), 0, None);
        assert_eq!(bre("**").unwrap().0, star);
        let anchored = Node::Concat(vec![Node::Look(Look::TextStart), lit(b'*')]);
        assert_eq!(bre("^*").unwrap().0, anchored);
    }

    #[test]
    fn basic_expressions_anchor_only_at_the_edges() {
        let (node, _) = bre("a^b$c").unwrap();
        let bytes = b"a^b$c".map(lit).to_vec();
        assert_eq!(node, Node::Concat(bytes));
        let (node, groups) = bre("\\(^a$\\)").unwrap();
        let inner = vec![
            Node::Look(Look::TextStart),
            lit(b'a'),
            Node::Look(Look::TextEnd),
        ];
        assert_eq!(node, Node::Group(Box::new(Node::Concat(inner)), 1));
        assert_eq!(groups, 1);
        // and the operators of extended expressions are ordinary there
        let (node, groups) = bre("(a|b)+").unwrap();
        assert_eq!(groups, 0);
        assert_eq!(node, Node::Concat(b"(a|b)+".map(lit).to_vec()));
    }

    #[test]
    fn bracket_expressions_read_ranges_classes_and_literal_brackets() {
        let set = |pat: &str| match ere(pat).unwrap().0 {
            Node::Set(set) => set,
            other => panic!("{pat}: {other:?}"),
        };
        assert_eq!(set("[]a]"), ByteSet::of(b"]a"));
        assert_eq!(set("[a-c-]"), ByteSet::of(b"abc-"));
        assert_eq!(set("[\\n]"), ByteSet::of(b"\\n"));
        assert_eq!(set("[[:digit:]x]").len(), 11);
        assert_eq!(set("[[.-.]-0]"), ByteSet::of(b"-./0"));
        assert_eq!(set("[^]a]").len(), 254);
        assert_eq!(set("[^[:space:]]").len(), 250);
        assert_eq!(ere("[b-a]"), Err(Error::Range));
        assert_eq!(ere("[a"), Err(Error::Bracket));
        assert_eq!(ere("[[:nope:]]"), Err(Error::Class));
        assert_eq!(ere("[[.ab.]]"), Err(Error::Collate));
    }

    #[test]
    fn parentheses_must_pair_except_a_lone_closing_one_in_an_extended_expression() {
        assert_eq!(ere("(a"), Err(Error::Paren));
        assert_eq!(
            ere("a)").unwrap().0,
            Node::Concat(vec![lit(b'a'), lit(b')')])
        );
        assert_eq!(bre("a\\)"), Err(Error::Paren));
        assert_eq!(ere("a\\"), Err(Error::Escape));
        let again = Node::Concat(vec![Node::Group(Box::new(lit(b'a')), 1), Node::BackRef(1)]);
        assert_eq!(ere("(a)\\1").unwrap().0, again);
        assert_eq!(bre("\\(a\\)\\1").unwrap().0, again);
        // only to a group that is complete
        for pat in ["\\1(a)", "(a\\1)", "(a)\\2", "\\1", "(a)|b\\1"] {
            assert_eq!(ere(pat), Err(Error::BackReference), "{pat}");
        }
    }

    #[test]
    fn ignoring_case_widens_literals_and_brackets() {
        let opts = Options {
            ignore_case: true,
            ..Options::default()
        };
        let (node, _) = parse(b"a[x-z]1", opts).unwrap();
        let sets = [&b"aA"[..], b"xyzXYZ", b"1"].map(|s| Node::Set(ByteSet::of(s)));
        assert_eq!(node, Node::Concat(sets.to_vec()));
    }
}
