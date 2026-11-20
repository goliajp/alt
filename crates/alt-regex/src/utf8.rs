//! What the single things of a pattern match in a UTF-8 text.

use crate::block::{node, remembered};
use crate::chars::{self, CharSet};
use crate::class::ByteSet;
use crate::syntax::Node;

fn span(from: u8, to: u8) -> Node {
    let mut set = ByteSet::empty();
    set.insert_range(from, to);
    Node::Set(set)
}

/// What matches the character or stray byte `rest` begins with, and how
/// many bytes of the pattern that is. Without regard to case a character
/// matches whatever reads as it in upper case; a stray byte only itself.
pub fn literal(rest: &[u8], ignore_case: bool) -> (Node, usize) {
    let Some((c, len)) = chars::decode(rest) else {
        return (Node::Set(ByteSet::byte(rest[0])), 1);
    };
    if ignore_case {
        // of the letters of ASCII only two have a third form
        let plain = u8::try_from(c).ok();
        let plain = plain.filter(|b| b.is_ascii() && !b"IiSs".contains(b));
        let alike = match plain {
            Some(b) => {
                let mut both = ByteSet::byte(b);
                both.fold_case();
                Node::Set(both)
            }
            None => node(&CharSet::alike(c)),
        };
        return (alike, len);
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
        return remembered(b".", u8::from(newline), || {
            let all = CharSet::new(vec![(1, u32::MAX)]);
            let gone: &[(u32, u32)] = if newline { &[(0x0a, 0x0a)] } else { &[] };
            all.without(gone)
        });
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
    remembered(name, 2 + u8::from(negated), || {
        let mut ranges = chars::class(name, false).expect("a known class");
        if name == b"alnum" {
            ranges.push((u32::from(b'_'), u32::from(b'_')));
        }
        let set = CharSet::new(ranges);
        match negated {
            true => set.negated(),
            false => set,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::tests::takes;

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
