//! Bracket expressions: the sets of bytes, or of characters in a UTF-8
//! text, written between `[` and `]`.

use crate::chars::{self, CharSet};
use crate::class::ByteSet;
use crate::syntax::Node;
use crate::{Error, Options, block};

/// One member of a bracket expression.
enum Element<'a> {
    /// A character, or in a text of bytes a byte.
    Char(u32),
    /// In a UTF-8 text, a byte that is no character: it matches nothing,
    /// and bounds a range as the character of its value.
    Stray(u8),
    /// A named class.
    Class(&'a [u8]),
    /// What is equivalent to this byte: itself.
    Like(u8),
}

fn element<'a>(pat: &'a [u8], at: &mut usize, utf8: bool) -> Result<Element<'a>, Error> {
    let rest = &pat[*at..];
    let byte = |b: u8| match utf8 && b >= 0x80 {
        true => Element::Stray(b),
        false => Element::Char(u32::from(b)),
    };
    let kind = match rest {
        [b'[', kind @ (b':' | b'=' | b'.'), ..] => *kind,
        [b, ..] => {
            let whole = chars::decode(rest).filter(|_| utf8);
            *at += whole.map_or(1, |(_, len)| len);
            return Ok(whole.map_or(byte(*b), |(c, _)| Element::Char(c)));
        }
        [] => return Err(Error::Bracket),
    };
    let body = &rest[2..];
    let Some(len) = body.windows(2).position(|w| w == [kind, b']']) else {
        return Err(Error::Bracket);
    };
    let name = &body[..len];
    *at += len + 4;
    match (kind, name) {
        (b':', _) => Ok(Element::Class(name)),
        (b'=', [b]) => Ok(Element::Like(*b)),
        (_, [b]) => Ok(byte(*b)),
        _ => Err(Error::Collate),
    }
}

/// The members read so far, as bytes or as characters; of characters
/// the classes by name.
enum Members<'a> {
    Bytes(ByteSet),
    Chars(Vec<(u32, u32)>, Vec<&'a [u8]>),
}

impl Members<'_> {
    fn add(&mut self, from: u32, to: u32) {
        match self {
            Members::Bytes(set) => set.insert_range(from as u8, to as u8),
            Members::Chars(ranges, _) => ranges.push((from, to)),
        }
    }
}

/// Reads a bracket expression behind its opening bracket. Says also
/// whether it is more than a list of ASCII characters.
pub fn parse(pat: &[u8], at: &mut usize, opts: Options) -> Result<(Node, bool), Error> {
    let written = *at;
    let negate = pat.get(*at) == Some(&b'^');
    *at += usize::from(negate);
    let mut members = match opts.utf8 {
        true => Members::Chars(Vec::new(), Vec::new()),
        false => Members::Bytes(ByteSet::empty()),
    };
    // the pattern of a UTF-8 text is read in upper case
    let cased = |c: u32| match opts.utf8 && opts.ignore_case {
        true => chars::upper(c),
        false => c,
    };
    let mut plain = !negate;
    let mut first = true;
    loop {
        match pat.get(*at) {
            None => return Err(Error::Bracket),
            Some(b']') if !first => break,
            Some(_) => {}
        }
        first = false;
        let (from, member) = match element(pat, at, opts.utf8)? {
            Element::Class(name) => {
                plain = false;
                match &mut members {
                    Members::Bytes(set) => set.union(&ByteSet::class(name).ok_or(Error::Class)?),
                    Members::Chars(_, classes) => {
                        ByteSet::class(name).ok_or(Error::Class)?;
                        classes.push(name);
                    }
                }
                continue;
            }
            Element::Like(b) => {
                if !opts.utf8 || b < 0x80 {
                    members.add(cased(u32::from(b)), cased(u32::from(b)));
                }
                continue;
            }
            Element::Char(c) => (cased(c), true),
            Element::Stray(b) => (u32::from(b), false),
        };
        // the locale orders no character outside ASCII
        let orders = |c: u32, member: bool| !opts.utf8 || !member || c < 0x80;
        let ranged = matches!(&pat[*at..], [b'-', next, ..] if *next != b']');
        if !ranged {
            if member {
                members.add(from, from);
            }
            plain &= from < 0x80 || !member;
            continue;
        }
        *at += 1;
        let to = match element(pat, at, opts.utf8)? {
            Element::Char(c) if orders(c, true) => cased(c),
            Element::Stray(b) => u32::from(b),
            Element::Char(_) => return Err(Error::Collate),
            _ => return Err(Error::Range),
        };
        if !orders(from, member) {
            return Err(Error::Collate);
        }
        if to < from {
            return Err(Error::Range);
        }
        members.add(from, to);
        plain = false;
    }
    *at += 1;
    let node = match members {
        Members::Bytes(mut set) => {
            if opts.ignore_case {
                set.fold_case();
            }
            if negate {
                set.negate();
                if opts.newline {
                    set.remove(b'\n');
                }
            }
            Node::Set(set)
        }
        Members::Chars(mut ranges, classes) => {
            let flags = 4 + u8::from(opts.ignore_case) + 2 * u8::from(opts.newline);
            block::remembered(&pat[written..*at], flags, || {
                for name in classes {
                    let class = chars::class(name, opts.ignore_case);
                    ranges.extend(class.expect("a known class"));
                }
                let mut set = CharSet::new(ranges);
                if opts.ignore_case {
                    set = set.folded();
                }
                if negate {
                    let newline: &[(u32, u32)] = if opts.newline { &[(0x0a, 0x0a)] } else { &[] };
                    set = set.negated().without(newline);
                }
                set
            })
        }
    };
    Ok((node, !plain))
}
