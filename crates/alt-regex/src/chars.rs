//! Characters of a UTF-8 text: decoding, sets of them, their classes and
//! their case, as the C library has them in a UTF-8 locale.

use std::sync::OnceLock;

use crate::class::ByteSet;
use crate::syntax::Look;
use crate::tables;

/// The last character the C library decodes: its UTF-8 goes on past
/// Unicode, to forms of six bytes.
const LAST: u32 = 0x7fff_ffff;
const SURROGATES: (u32, u32) = (0xd800, 0xdfff);

/// The character `bytes` begin with and its length, when they begin with
/// one: no longer form of a shorter one and no surrogate.
pub fn decode(bytes: &[u8]) -> Option<(u32, usize)> {
    let tail = |i: usize| match bytes.get(i) {
        Some(&b @ 0x80..=0xbf) => Some(u32::from(b & 0x3f)),
        _ => None,
    };
    let lead = u32::from(*bytes.first()?);
    let (c, len) = match lead {
        0..=0x7f => (lead, 1),
        0xc2..=0xdf => ((lead & 0x1f) << 6 | tail(1)?, 2),
        0xe0..=0xef => ((lead & 0x0f) << 12 | tail(1)? << 6 | tail(2)?, 3),
        0xf0..=0xfd => {
            let len = (lead << 24).leading_ones() as usize;
            let mut c = lead & (0x7f >> len);
            for i in 1..len {
                c = c << 6 | tail(i)?;
            }
            (c, len)
        }
        _ => return None,
    };
    let least = [0, 0, 0x80, 0x800, 0x10000, 0x20_0000, 0x400_0000][len];
    let fine = c >= least && !(SURROGATES.0..=SURROGATES.1).contains(&c);
    fine.then_some((c, len))
}

fn within(ranges: &[(u32, u32)], c: u32) -> bool {
    let at = ranges.partition_point(|&(_, to)| to < c);
    ranges.get(at).is_some_and(|&(from, _)| from <= c)
}

/// The upper-case form of `c`, or `c`.
pub fn upper(c: u32) -> u32 {
    match tables::TO_UPPER.binary_search_by_key(&c, |&(lower, _)| lower) {
        Ok(at) => tables::TO_UPPER[at].1,
        Err(_) => c,
    }
}

/// The characters whose upper-case form is `c`, other than `c`.
fn lower(c: u32) -> impl Iterator<Item = u32> {
    static BY_UPPER: OnceLock<Vec<(u32, u32)>> = OnceLock::new();
    let by_upper = BY_UPPER.get_or_init(|| {
        let mut pairs: Vec<_> = tables::TO_UPPER.iter().map(|&(l, u)| (u, l)).collect();
        pairs.sort_unstable();
        pairs
    });
    let from = by_upper.partition_point(|&(upper, _)| upper < c);
    by_upper[from..]
        .iter()
        .take_while(move |&&(upper, _)| upper == c)
        .map(|&(_, lower)| lower)
}

/// Whether `c` is part of a word: a letter, a digit or `_`.
pub fn word(c: u32) -> bool {
    match u8::try_from(c) {
        Ok(b) if b < 0x80 => b.is_ascii_alphanumeric() || b == b'_',
        _ => within(tables::ALPHA, c),
    }
}

/// The character at `at`; a byte that begins none counts as the
/// character of its value, as the C library has it.
fn at(text: &[u8], at: usize) -> u32 {
    decode(&text[at..]).map_or(u32::from(text[at]), |(c, _)| c)
}

/// Where the character that holds the byte at `at` begins and ends, for
/// a byte that could go on one.
fn around(text: &[u8], at: usize) -> Option<(usize, usize)> {
    let mut from = at;
    while from > 0 && at - from < 3 && text[from] & 0xc0 == 0x80 {
        from -= 1;
    }
    let (_, len) = decode(&text[from..])?;
    (from + len > at).then_some((from, from + len))
}

/// Whether `pos` lies inside a character.
pub fn inside(text: &[u8], pos: usize) -> bool {
    pos > 0
        && pos < text.len()
        && text[pos] & 0xc0 == 0x80
        && around(text, pos).is_some_and(|(from, _)| from < pos)
}

/// As [`crate::pike::holds`] for the edges of words in a UTF-8 text.
/// Inside a character the same one lies on both sides.
pub fn holds(look: Look, text: &[u8], pos: usize) -> bool {
    if inside(text, pos) {
        return look == Look::NotWordBoundary;
    }
    let before = pos > 0 && {
        let last = match around(text, pos - 1) {
            Some((from, to)) if to == pos => from,
            _ => pos - 1,
        };
        word(at(text, last))
    };
    let after = pos < text.len() && word(at(text, pos));
    match look {
        Look::WordBoundary => before != after,
        Look::NotWordBoundary => before == after,
        Look::WordStart => !before && after,
        Look::WordEnd => before && !after,
        _ => unreachable!("only the edges of words depend on the characters"),
    }
}

/// How many bytes at the start of `text` are, character by character and
/// in either case, what `group` is; `None` when `text` begins otherwise.
pub fn again(text: &[u8], group: &[u8]) -> Option<usize> {
    let (mut taken, mut done) = (0, 0);
    while done < group.len() {
        let (theirs, step) = decode(&group[done..]).unwrap_or((u32::MAX, 1));
        let (ours, len) = decode(text.get(taken..)?).unwrap_or((u32::MAX, 1));
        let same = match theirs {
            u32::MAX => ours == u32::MAX && text.get(taken) == Some(&group[done]),
            _ => upper(ours) == upper(theirs),
        };
        if !same {
            return None;
        }
        (taken, done) = (taken + len, done + step);
    }
    Some(taken)
}

/// A POSIX character class by name. Without regard to case, `upper` and
/// `lower` are every letter.
pub fn class(name: &[u8], ignore_case: bool) -> Option<Vec<(u32, u32)>> {
    let ascii = |name| {
        let set = ByteSet::class(name).expect("a known class");
        let members = (0..0x80u8).filter(move |&b| set.contains(b));
        members.map(|b| (u32::from(b), u32::from(b))).collect()
    };
    Some(match name {
        b"upper" | b"lower" if ignore_case => tables::ALPHA.to_vec(),
        b"alpha" => tables::ALPHA.to_vec(),
        b"upper" => tables::UPPER.to_vec(),
        b"lower" => tables::LOWER.to_vec(),
        b"space" => tables::SPACE.to_vec(),
        b"blank" => tables::BLANK.to_vec(),
        b"punct" => tables::PUNCT.to_vec(),
        b"print" => tables::PRINT.to_vec(),
        b"graph" => tables::GRAPH.to_vec(),
        b"cntrl" => tables::CNTRL.to_vec(),
        b"digit" | b"xdigit" => ascii(name),
        b"alnum" => {
            let mut all = tables::ALPHA.to_vec();
            all.push((u32::from(b'0'), u32::from(b'9')));
            all
        }
        _ => return None,
    })
}

/// A set of characters: ranges in order, apart from one another.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct CharSet(Vec<(u32, u32)>);

impl CharSet {
    pub fn new(mut ranges: Vec<(u32, u32)>) -> Self {
        ranges.sort_unstable();
        let mut out: Vec<(u32, u32)> = Vec::with_capacity(ranges.len());
        for (from, to) in ranges {
            match out.last_mut() {
                Some(last) if from <= last.1.saturating_add(1) => last.1 = last.1.max(to),
                _ => out.push((from, to.min(LAST))),
            }
        }
        CharSet(out).without(&[SURROGATES])
    }

    pub fn ranges(&self) -> &[(u32, u32)] {
        &self.0
    }

    pub fn contains(&self, c: u32) -> bool {
        within(&self.0, c)
    }

    /// The set without the characters of `gone`, which are in order.
    pub fn without(self, gone: &[(u32, u32)]) -> Self {
        let mut out = Vec::with_capacity(self.0.len());
        let mut gone = gone.iter().peekable();
        for (mut from, to) in self.0 {
            while let Some(&&(low, high)) = gone.peek() {
                if low > to {
                    break;
                }
                if high >= from {
                    if low > from {
                        out.push((from, low - 1));
                    }
                    from = high.saturating_add(1);
                }
                if high > to {
                    break;
                }
                gone.next();
            }
            if from <= to {
                out.push((from, to));
            }
        }
        CharSet(out)
    }

    /// Every character that is not in the set.
    pub fn negated(self) -> Self {
        CharSet(vec![(0, LAST)])
            .without(&self.0)
            .without(&[SURROGATES])
    }

    /// The characters whose upper-case form is in the set: what a text
    /// matches when both it and the pattern are read in upper case.
    pub fn folded(self) -> Self {
        let (mut more, mut gone) = (Vec::new(), Vec::new());
        for &(c, upper) in tables::TO_UPPER {
            match (self.contains(c), self.contains(upper)) {
                (true, false) => gone.push((c, c)),
                (false, true) => more.push((c, c)),
                _ => {}
            }
        }
        let mut ranges = self.without(&gone).0;
        ranges.extend(more);
        CharSet::new(ranges)
    }

    /// The characters that read as `c` when case is ignored.
    pub fn alike(c: u32) -> Self {
        let upper = upper(c);
        let mut ranges = vec![(upper, upper)];
        ranges.extend(lower(upper).map(|c| (c, c)));
        CharSet::new(ranges)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_well_formed_characters_decode() {
        assert_eq!(decode(b"a"), Some((0x61, 1)));
        assert_eq!(decode("é".as_bytes()), Some((0xe9, 2)));
        assert_eq!(decode("€x".as_bytes()), Some((0x20ac, 3)));
        assert_eq!(decode("😀".as_bytes()), Some((0x1f600, 4)));
        // past Unicode, as the C library reads them
        assert_eq!(decode(b"\xf4\x90\x80\x80"), Some((0x11_0000, 4)));
        assert_eq!(decode(b"\xf8\x88\x80\x80\x80"), Some((0x20_0000, 5)));
        assert_eq!(decode(b"\xfd\xbf\xbf\xbf\xbf\xbf"), Some((LAST, 6)));
        for bad in [
            &b"\xe9"[..],
            b"\x80",
            b"\xc0\x80",
            b"\xe0\x80\x80",
            b"\xed\xa0\x80",
            b"\xf0\x8f\xbf\xbf",
            b"\xf8\x87\xbf\xbf\xbf",
            b"\xfe\x80",
            b"\xe2\x82",
            b"",
        ] {
            assert_eq!(decode(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn sets_merge_subtract_and_negate() {
        let set = CharSet::new(vec![(5, 9), (1, 3), (4, 4), (20, 30), (0xd700, 0xe000)]);
        let whole = [(1, 9), (20, 30), (0xd700, 0xd7ff), (0xe000, 0xe000)];
        assert_eq!(set.ranges(), whole);
        let less = set.clone().without(&[(0, 1), (8, 22), (30, 40)]);
        assert_eq!(less.ranges()[..2], [(2, 7), (23, 29)]);
        let rest = set.negated();
        let gaps = [(0, 0), (10, 19), (31, 0xd6ff), (0xe001, LAST)];
        assert_eq!(rest.ranges(), gaps);
        assert_eq!(rest.negated().ranges(), whole);
    }

    #[test]
    fn case_is_ignored_by_reading_in_upper_case() {
        assert_eq!(upper(0xe9), 0xc9);
        assert_eq!(upper(0xc9), 0xc9);
        // the dotless i and the long s read as I and S
        assert_eq!(
            CharSet::alike(0x69).ranges(),
            [(0x49, 0x49), (0x69, 0x69), (0x131, 0x131)]
        );
        assert_eq!(
            CharSet::alike(0x17f).ranges(),
            [(0x53, 0x53), (0x73, 0x73), (0x17f, 0x17f)]
        );
        let lower = CharSet::new(vec![(0x61, 0x7a)]).folded();
        assert!(lower.ranges().is_empty());
        let upper = CharSet::new(vec![(0x41, 0x5a)]).folded();
        assert!(upper.contains(0x61) && upper.contains(0x131) && !upper.contains(0xe9));
    }

    #[test]
    fn the_edges_of_words_follow_the_characters() {
        let text = "né e".as_bytes();
        assert!(!holds(Look::WordBoundary, text, 1));
        assert!(!holds(Look::WordBoundary, text, 2));
        assert!(holds(Look::NotWordBoundary, text, 2));
        assert!(inside(text, 2) && !inside(text, 1) && !inside(text, 3));
        assert!(!inside(b"\xa9\xa9", 1));
        assert!(holds(Look::WordEnd, text, 3));
        assert!(holds(Look::WordStart, text, 4));
        // a byte that is no character counts as the one of its value
        assert!(!holds(Look::WordEnd, b"n\xe9", 1));
        assert!(holds(Look::WordEnd, b"n\xa9", 1));
    }

    #[test]
    fn a_group_is_met_again_character_by_character() {
        assert_eq!(again("ÉX".as_bytes(), "é".as_bytes()), Some(2));
        assert_eq!(again("ıx".as_bytes(), b"I"), Some(2));
        assert_eq!(again(b"\xe9", b"\xe9"), Some(1));
        assert_eq!(again(b"\xc9", b"\xe9"), None);
        assert_eq!(again(b"a", b"ab"), None);
        // the text ends where a byte that is no character is due
        assert_eq!(again(b"", b"\xe9"), None);
        assert_eq!(again(b"a", b"a\xe9"), None);
    }
}
