//! The cases the crate's documentation lists as answered otherwise by
//! the GNU C library: what this crate answers in each.

use alt_regex::{Options, Regex};

type Spans = Option<&'static [Option<(usize, usize)>]>;

/// Whether case is ignored and whether the text is UTF-8, a pattern, a
/// text, and the match with its groups.
type Case = (bool, bool, &'static [u8], &'static [u8], Spans);

pub const CASES: [Case; 15] = [
    // an anchor or the edge of a word inside or behind a repetition
    (false, false, b"a*\\B", b"xa", Some(&[Some((1, 1))])),
    (false, false, b"(\\ba){2,}", b"aaa", None),
    (
        false,
        false,
        b"(^a?){1,2}",
        b"a",
        Some(&[Some((0, 1)), Some((0, 1))]),
    ),
    // the groups in a repetition without bound of what may match nothing
    (
        false,
        false,
        b"(b{0,1}|[ab]{0,1})*a?",
        b"bbaxaa",
        Some(&[Some((0, 3)), Some((2, 3))]),
    ),
    (
        false,
        false,
        b"(()+(b{0,1}|ab*x?b?){2,})*",
        b"aaab",
        Some(&[Some((0, 4)), Some((0, 4)), Some((0, 0)), Some((2, 4))]),
    ),
    (
        false,
        false,
        b"((b*)|a)*",
        b"a",
        Some(&[Some((0, 1)), Some((0, 1)), None]),
    ),
    (
        false,
        false,
        b"((x|)*b){,2}",
        b"xb",
        Some(&[Some((0, 2)), Some((0, 2)), Some((0, 1))]),
    ),
    // such a group referred back to
    (
        false,
        false,
        b"(a*)*\\1",
        b"aaa",
        Some(&[Some((0, 3)), Some((3, 3))]),
    ),
    // a character with a shorter upper-case form, then a byte that is
    // no character
    (true, true, b"x", b"\xc5\xbfx\xf0", Some(&[Some((2, 3))])),
    (true, true, b"b+", b" \xc4\xb1ab\xe9", Some(&[Some((4, 5))])),
    // a byte of the pattern that begins a character of the text
    (false, true, b"\xf0[^a]", b"bx\xf0\x9f\x98\x80", None),
    (
        false,
        true,
        b"^(caf\xe9[^(]*)",
        b"caf\xe9\x9c\x80 x",
        Some(&[Some((0, 4)), Some((0, 4))]),
    ),
    (
        false,
        true,
        b"\xf0+[^[:space:]b]?|ab?",
        b"\xf0\x9f\x98\x80a",
        Some(&[Some((0, 1))]),
    ),
    // such a byte in the pattern, and the groups
    (
        false,
        true,
        b"(.*).\xc3+",
        b"a\xc3\x9fbb",
        Some(&[Some((0, 2)), Some((0, 0))]),
    ),
    // `.*` before a byte that goes on a character
    (false, true, b".*\xa9", b"\xc3\xa9", Some(&[Some((1, 2))])),
];

/// The match of `pattern` in `text` with all its groups.
pub fn ours(
    ignore_case: bool,
    utf8: bool,
    pattern: &[u8],
    text: &[u8],
) -> Option<Vec<Option<(usize, usize)>>> {
    let options = Options {
        ignore_case,
        utf8,
        ..Options::default()
    };
    let re = Regex::with_options(pattern, options).unwrap();
    let found = re.captures(text);
    assert_eq!(re.is_match(text), found.is_some());
    assert_eq!(re.find(text), found.as_ref().and_then(|found| found.get(0)));
    let span = |found: &alt_regex::Captures, group| found.get(group).map(|r| (r.start, r.end));
    found.map(|found| (0..=re.groups()).map(|group| span(&found, group)).collect())
}

#[test]
fn the_cases_the_c_library_answers_otherwise_are_answered_as_documented() {
    for (ignore_case, utf8, pattern, text, expected) in CASES {
        let found = ours(ignore_case, utf8, pattern, text);
        let shown = format!("{} on {}", pattern.escape_ascii(), text.escape_ascii());
        assert_eq!(found.as_deref(), expected, "{shown}");
    }
}
