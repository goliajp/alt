//! What a backslash, a bracket and a parenthesis begin.

use super::{Look, MAX_DEPTH, MAX_GROUPS, Node, Parser};
use crate::class::ByteSet;
use crate::{Error, bracket, utf8};

impl Parser<'_> {
    pub(super) fn escape(&mut self) -> Result<Node, Error> {
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

    pub(super) fn bracket(&mut self) -> Result<Node, Error> {
        let (node, complex) = bracket::parse(self.pat, &mut self.at, self.opts)?;
        self.complex |= complex;
        Ok(node)
    }

    /// A group, behind its opening parenthesis.
    pub(super) fn group(&mut self) -> Result<Node, Error> {
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
        self.tall += 1;
        self.complete.push(number);
        self.budget.take(size_of::<Node>())?;
        Ok(Node::Group(Box::new(inner), number))
    }
}
