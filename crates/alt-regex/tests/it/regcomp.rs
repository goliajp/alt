//! The same patterns and texts given to this crate and to the GNU C
//! library's `regcomp` and `regexec`: both must accept the same patterns
//! and report the same match. The GNU C library is the reference on
//! every system, so these tests are built only where it is the C library.

use std::ffi::{CString, c_char, c_int, c_void};

mod groups;
mod judged;
mod oracle;
mod random;
mod utf8;

use alt_regex::{Options, Regex, Syntax};

/// Room for the C library's compiled pattern, which is smaller.
#[repr(C, align(16))]
struct Compiled([u8; 256]);

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug)]
struct Span {
    from: c_int,
    to: c_int,
}

unsafe extern "C" {
    fn regcomp(compiled: *mut Compiled, pattern: *const c_char, flags: c_int) -> c_int;
    fn regexec(
        compiled: *const Compiled,
        text: *const c_char,
        spans: usize,
        span: *mut Span,
        flags: c_int,
    ) -> c_int;
    fn regfree(compiled: *mut Compiled);
    fn newlocale(mask: c_int, locale: *const c_char, base: *mut c_void) -> *mut c_void;
    fn uselocale(locale: *mut c_void) -> *mut c_void;
}

/// Puts the calling thread in a UTF-8 locale, where the C library
/// matches character by character. Code points order its ranges there.
fn in_utf8_locale() {
    const CTYPE_AND_COLLATE: c_int = 1 << 0 | 1 << 3;
    // SAFETY: the name ends in NUL, and the locale is never freed
    let made = unsafe { newlocale(CTYPE_AND_COLLATE, c"C.UTF-8".as_ptr(), std::ptr::null_mut()) };
    assert!(!made.is_null(), "the locale C.UTF-8 is not installed");
    // SAFETY: a locale that newlocale made
    unsafe { uselocale(made) };
}

const EXTENDED: c_int = 1;
const ICASE: c_int = 2;
const NEWLINE: c_int = 4;
const GROUPS: usize = 10;

/// The C library's verdict on a pattern.
struct Theirs(Option<Compiled>);

impl Theirs {
    fn new(pattern: &[u8], options: Options) -> Self {
        let mut flags = 0;
        if options.syntax == Syntax::Extended {
            flags |= EXTENDED;
        }
        if options.ignore_case {
            flags |= ICASE;
        }
        if options.newline {
            flags |= NEWLINE;
        }
        let pattern = CString::new(pattern).unwrap();
        let mut compiled = Compiled([0; 256]);
        // SAFETY: the buffer is larger than regex_t and the pattern ends in NUL
        let refused = unsafe { regcomp(&mut compiled, pattern.as_ptr(), flags) };
        Theirs((refused == 0).then_some(compiled))
    }

    fn spans(&self, text: &[u8]) -> Option<Vec<Option<(usize, usize)>>> {
        self.registers(text, GROUPS)
    }

    /// What `regexec` puts into `count` registers.
    fn registers(&self, text: &[u8], count: usize) -> Option<Vec<Option<(usize, usize)>>> {
        let text = CString::new(text).unwrap();
        let mut spans = [Span { from: -1, to: -1 }; GROUPS];
        let compiled = self.0.as_ref().unwrap();
        // SAFETY: compiled by regcomp, and the array holds GROUPS spans
        let missed = unsafe { regexec(compiled, text.as_ptr(), count, spans.as_mut_ptr(), 0) };
        if missed != 0 {
            return None;
        }
        let span = |s: &Span| (s.from >= 0).then_some((s.from as usize, s.to as usize));
        Some(spans[..count].iter().map(span).collect())
    }
}

impl Drop for Theirs {
    fn drop(&mut self) {
        if let Some(compiled) = &mut self.0 {
            // SAFETY: compiled by regcomp and freed once
            unsafe { regfree(compiled) };
        }
    }
}

/// Both must accept or refuse each pattern, and find the same match in
/// each of a few texts.
fn accepted_alike(cases: &[(&str, Options)]) {
    for &(pat, options) in cases {
        let theirs = Theirs::new(pat.as_bytes(), options);
        let ours = Regex::with_options(pat.as_bytes(), options);
        assert_eq!(
            ours.is_ok(),
            theirs.0.is_some(),
            "{pat:?}: ours {:?}",
            ours.as_ref().err()
        );
        let Ok(ours) = ours else { continue };
        let texts = [
            "", "a", "*a", "aa", "ab", "b", "a+b?", "a|b", "aab", "a)", "]", "x ab.", "cab a b",
            "ab ", "_", "B", "-", "z", "Z", "^", "`", "^a", "^^", "a^", "a**",
        ];
        for text in texts {
            let expected = theirs.spans(text.as_bytes()).map(|spans| spans[0]);
            let got = ours.find(text.as_bytes()).map(|r| Some((r.start, r.end)));
            assert_eq!(got, expected, "{pat:?} on {text:?}");
        }
    }
}

#[test]
fn hand_picked_patterns_are_accepted_or_refused_as_the_c_library_does() {
    let basic = Options {
        syntax: Syntax::Basic,
        ..Options::default()
    };
    accepted_alike(&[
        ("*a", Options::default()),
        ("a**", Options::default()),
        ("a|*b", Options::default()),
        ("a||b", Options::default()),
        ("()", Options::default()),
        ("a{1", Options::default()),
        ("a{1,2}{3}", Options::default()),
        ("a{2,1}", Options::default()),
        ("a{,}", Options::default()),
        ("{1}", Options::default()),
        ("a)", Options::default()),
        ("(a", Options::default()),
        ("[a", Options::default()),
        ("[]", Options::default()),
        ("[]]", Options::default()),
        ("[z-a]", Options::default()),
        ("[[:nope:]]", Options::default()),
        ("a\\", Options::default()),
        ("^*", Options::default()),
        ("$*", Options::default()),
        ("a{0}b", Options::default()),
        ("*a", basic),
        ("^*a", basic),
        ("\\(*a\\)", basic),
        ("a\\{1,2\\}", basic),
        ("a\\{1", basic),
        ("\\(a", basic),
        ("a\\)", basic),
        ("a\\+b\\?", basic),
        ("a\\|b", basic),
        // no star and no interval behind another repetition
        ("^[a-z ]**(", basic),
        ("a*\\{1,2\\}", basic),
        ("a\\+*", basic),
        ("a\\{1,2\\}\\{3\\}", basic),
        ("a\\{0\\}*", basic),
        ("a*\\+", basic),
        ("a\\{1,2\\}\\?", basic),
        ("^**", basic),
        // a circumflex anchors only where a branch begins
        ("^^", basic),
        ("^^a", basic),
        ("\\b^", basic),
        ("\\<^a", basic),
        ("^^\\{1,2\\}", basic),
        ("^\\{1,2\\}", basic),
        ("b\\|^a", basic),
        ("\\(^a\\)", basic),
        ("\\(a\\)^", basic),
        ("\\bab\\b", Options::default()),
        ("\\<a", Options::default()),
        ("b\\>", Options::default()),
        ("a\\Bb", Options::default()),
        ("\\w+\\W\\s?", Options::default()),
    ]);
}

#[test]
fn hyphens_and_ranges_inside_brackets_are_read_as_the_c_library_reads_them() {
    let any_case = Options {
        ignore_case: true,
        ..Options::default()
    };
    accepted_alike(&[
        // a hyphen stands for itself only first or last in its brackets
        ("[[:alpha:]-z]", Options::default()),
        ("[[=a=]-z]", Options::default()),
        ("[a-b-c]", Options::default()),
        ("[a-a-z]", Options::default()),
        ("[--z]", Options::default()),
        ("[^-z]", Options::default()),
        ("[a-z-]", Options::default()),
        ("[[:alpha:]-]", Options::default()),
        ("[]-a]", Options::default()),
        // without regard to case a range is read in upper case
        ("[A-z]", any_case),
        ("[_-z]", any_case),
        ("[a-Z]", any_case),
        ("[Z-a]", any_case),
        ("[a-_]", any_case),
        ("[^[:lower:]_-a]", any_case),
    ]);
}

#[test]
fn back_references_match_as_the_c_library_matches_them() {
    let extended = Options::default();
    let basic = Options {
        syntax: Syntax::Basic,
        ..extended
    };
    let any_case = Options {
        ignore_case: true,
        ..extended
    };
    let cases = [
        ("(a)\\1", extended),
        ("(a*)\\1", extended),
        ("^(.+)\\1$", extended),
        ("(a|ab)\\1", extended),
        ("(a)(b)\\2\\1", extended),
        ("(a*)*\\1", extended),
        ("(a|b)*\\1", extended),
        ("(a)\\1*", extended),
        ("(a)\\1{2}", extended),
        ("(ab|a)(bc|c)?\\1", extended),
        ("(a)|b\\1", extended),
        ("((a)|b)\\2", extended),
        ("((a)|b)*\\2", extended),
        ("(a)\\1", any_case),
        ("([a-c]+)x\\1", any_case),
        ("\\(a\\)\\1", basic),
        ("\\(a*\\)b\\1", basic),
        ("\\(a\\)\\(b\\)*\\2", basic),
        ("\\1(a)", extended),
        ("(a\\1)", extended),
        ("(a)\\2", extended),
        ("\\1", basic),
    ];
    let texts = [
        "", "a", "aa", "aaa", "aaaa", "aaaaa", "ab", "abab", "abba", "aba", "abcabc", "b", "bb",
        "xaab", "aabaa", "aA", "Aa", "abcab", "abcbc", "abxAB", "ba", "abb",
    ];
    let mut compared = 0;
    let mut differ = Vec::new();
    for (pat, options) in cases {
        let theirs = Theirs::new(pat.as_bytes(), options);
        let ours = Regex::with_options(pat.as_bytes(), options);
        assert_eq!(ours.is_ok(), theirs.0.is_some(), "{pat:?}");
        let Ok(ours) = ours else { continue };
        for text in texts {
            let expected = theirs.spans(text.as_bytes());
            // for a repeated group that may be empty the C library reports
            // spans that no match has, one with an end before its start:
            // there only the whole match is compared
            let groups = if pat == "(a*)*\\1" { 0 } else { ours.groups() };
            let expected = expected.map(|spans| spans[..=groups].to_vec());
            let found = ours.captures(text.as_bytes()).map(|found| {
                let span = |i| found.get(i).map(|r| (r.start, r.end));
                (0..=groups).map(span).collect::<Vec<_>>()
            });
            if found != expected {
                differ.push(format!("{pat:?} on {text:?}: {found:?} not {expected:?}"));
            }
            assert_eq!(ours.is_match(text.as_bytes()), found.is_some());
            compared += 1;
        }
    }
    assert_eq!(compared, 17 * texts.len());
    assert!(differ.is_empty(), "{}", differ.join("\n"));
}

/// Git asks for the first group only. The C library then keeps no
/// other, and a reference to another is what its walk through the
/// match cannot follow.
#[test]
fn a_reference_to_a_group_not_asked_for_is_answered_as_the_c_library_answers() {
    let patterns = [
        // no choice in the pattern: the reference is taken to be empty
        "((a)\\2)",
        "(a)()\\2",
        "(a)(b)\\2",
        "(a)(b{0})\\2b",
        // a choice: the reference fails, and so does the search
        "(a)(b?)\\2",
        "(a)(b)\\2c*",
        "(a)(b)\\2|(a)b",
        "(a+)(b)\\2|ab",
        "(a)(b)\\2|(c)",
        "^([a-z]+) ([a-z]+) \\2",
        // unless the walk ends before it comes to the reference
        "(x*)*(a)\\2",
        "(a)(B?)*_(.+\\2)+",
        // a turn that takes nothing while the group is open
        "(a|x*)*(b)\\2",
        "(a(x+)*)(b)\\3",
        // the group asked for is referred to as well
        "(a*)(b)\\2\\1",
        "(a)\\1(b)\\2",
    ];
    let texts = [
        "",
        "a",
        "aa",
        "ab",
        "abb",
        "abbb",
        "abbc",
        "aabb",
        "abba",
        "abbaa",
        "xabb",
        "b",
        "bb",
        "a_b1aa1A",
        " _a_b1aa1A",
        "a00b0AB_",
        "ab ab ab",
        "ab cd cd",
        "axbb",
        "abc",
        "axb",
    ];
    let mut differ = Vec::new();
    for pattern in patterns {
        let theirs = Theirs::new(pattern.as_bytes(), Options::default());
        let ours = Regex::new(pattern.as_bytes()).unwrap();
        for text in texts {
            let expected = theirs.registers(text.as_bytes(), 2);
            // a group the C library gives a start and no end is none
            let whole = |span: &Option<(usize, usize)>| span.filter(|span| span.0 <= span.1);
            let expected = expected.map(|spans| spans.iter().map(whole).collect::<Vec<_>>());
            let found = ours.captures_upto(text.as_bytes(), 1).map(|found| {
                let span = |i| found.get(i).map(|r| (r.start, r.end));
                vec![span(0), span(1)]
            });
            if found != expected {
                differ.push(format!(
                    "{pattern:?} on {text:?}: {found:?} not {expected:?}"
                ));
            }
        }
    }
    assert!(differ.is_empty(), "{}", differ.join("\n"));
    // where the walk of the C library ends with the group begun and
    // not ended, there is no match here
    let pattern = b"(a(x*)*)(b)\\2";
    let theirs = Theirs::new(pattern, Options::default()).registers(b"axb", 2);
    assert_eq!(theirs, Some(vec![Some((0, 3)), Some((0, usize::MAX))]));
    assert!(
        Regex::new(pattern)
            .unwrap()
            .captures_upto(b"axb", 1)
            .is_none()
    );
}

/// The list of differences in the documentation stays true of the C
/// library at hand: a case it has come to answer as this crate does is
/// no difference any more.
#[test]
fn the_c_library_answers_otherwise_where_the_documentation_says_so() {
    for (ignore_case, utf8, pattern, text, _) in crate::differences::CASES {
        let options = Options {
            ignore_case,
            utf8,
            ..Options::default()
        };
        // on a thread of its own, which alone is put in the locale
        let theirs = std::thread::spawn(move || {
            if utf8 {
                in_utf8_locale();
            }
            Theirs::new(pattern, options).spans(text)
        });
        let theirs = theirs.join().unwrap();
        let ours = crate::differences::ours(ignore_case, utf8, pattern, text);
        let theirs =
            theirs.map(|spans| spans[..=ours.as_ref().map_or(0, |ours| ours.len() - 1)].to_vec());
        let shown = format!("{} on {}", pattern.escape_ascii(), text.escape_ascii());
        assert_ne!(ours, theirs, "{shown}");
    }
}
