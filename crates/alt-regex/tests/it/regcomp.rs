//! The same patterns and texts given to this crate and to the GNU C
//! library's `regcomp` and `regexec`: both must accept the same patterns
//! and report the same match. The GNU C library is the reference on
//! every system, so these tests are built only where it is the C library.

use std::ffi::{CString, c_char, c_int, c_void};

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
        let text = CString::new(text).unwrap();
        let mut spans = [Span { from: -1, to: -1 }; GROUPS];
        let compiled = self.0.as_ref().unwrap();
        // SAFETY: compiled by regcomp, and the array holds GROUPS spans
        let missed = unsafe { regexec(compiled, text.as_ptr(), GROUPS, spans.as_mut_ptr(), 0) };
        if missed != 0 {
            return None;
        }
        let span = |s: &Span| (s.from >= 0).then_some((s.from as usize, s.to as usize));
        Some(spans.iter().map(span).collect())
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

#[test]
fn hand_picked_patterns_are_accepted_or_refused_as_the_c_library_does() {
    let basic = Options {
        syntax: Syntax::Basic,
        ..Options::default()
    };
    let cases: [(&str, Options); 35] = [
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
        ("\\bab\\b", Options::default()),
        ("\\<a", Options::default()),
        ("b\\>", Options::default()),
        ("a\\Bb", Options::default()),
        ("\\w+\\W\\s?", Options::default()),
    ];
    for (pat, options) in cases {
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
            "ab ",
        ];
        for text in texts {
            let expected = theirs.spans(text.as_bytes()).map(|spans| spans[0]);
            let got = ours.find(text.as_bytes()).map(|r| Some((r.start, r.end)));
            assert_eq!(got, expected, "{pat:?} on {text:?}");
        }
    }
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

/// Patterns whose groups could lie in more than one place.
const AMBIGUOUS: [(&str, &str); 44] = [
    ("(a|ab)(c|bcd)(d*)", "abcd"),
    ("^([a-z]+|[a-z]+ [a-z]+)( .*)?$", "foo bar baz"),
    ("(a|ab)(bc|c)?", "abc"),
    ("((a)|b)*", "ab"),
    ("(a*)*", "b"),
    ("(a*)+", "b"),
    ("(a*)*b", "aab"),
    // fewer copies than a count allows
    ("(.*)?ab{0,1}(.+){,2}$", "bbcca.c x"),
    ("(.ab{2})*([^ab]+){,2}", "cxaa.cba "),
    ("^(([^a]+)+){,2}.+", "bxc.ba. "),
    ("c\\W?(([b-c]{,2}.[^ab]+){1,2}){,2}", "axcxxa "),
    ("b*|([ab]{0,1}[^a]{1,2}[^ab]+){,2}[^[:space:]b]?", "bc. .bb"),
    // a turn of a repetition that takes nothing
    ("b(a{0,1}){1,2}", "abacc."),
    ("(\\(*[b-c]?){1,2}$", "bab"),
    ("((c*)?\\({,2}(.?){2}){1,2}a{0,1}", "ca"),
    (
        "((a{,2}a{0,1})?){1,2}(.{1,2}a*)?(.?[]a]+\\(){0,1}$",
        "bb. baa b",
    ),
    ("^(a{2}([^[:space:]b]?\\.{0,1}){1,2}){1,2} {,2}", "aacb"),
    ("([[:alpha:]]{2,}|c{,2}c{,2}x?){2,}x{0,1}", "x.a .b"),
    ("(\\s{0,1}b*[ab]*){1,2}$", "a"),
    // a copy of a repetition, in which no group is marked
    ("((c*)?\\({,2}(.?){2}){1,2}a{0,1}", "b abax"),
    ("(\\w{,2}[ab]{,2}){,2}", "b b."),
    ("^(b?[^a]{,2}){2}([a-c]?){,2}", ".a bba.x"),
    ("[[:alpha:]]((b*){1,2}a[^a]{0,1})+(c*)", "aa.ab."),
    ("([ab](c{0,1}){1,2}.{0,1}){2,}\\.*", ".a.b ab"),
    // a repetition that takes nothing at all
    ("\\.?( ab{2,}[[:alpha:]]|b?c{0,1})*", "a"),
    ("^.{2}(([^a]{1,2})+[a-c]{1,2}c*|c{,2})*[a-c]?", "bab"),
    ("(a?)*(b?)+", "x"),
    // a branch that ends behind a condition, with nothing taken since,
    // gives way to one that does not
    ("(a)$|a", "a"),
    ("(a\\>)|a", "a"),
    ("^(a*)|b", "a"),
    ("(^)|x*", ""),
    ("(^)|($)", ""),
    ("($)|(^)", ""),
    ("(a)$|(a)\\>", "a"),
    ("(^a)|(a)", "a"),
    ("(a$)|(a)", "a"),
    ("(a)($)|(a)", "a"),
    ("((a)$)|(a)", "a"),
    ("(a$|a)", "a"),
    ("(a$|a)($)", "a"),
    ("x(^)|x()", "x"),
    ("^a*a*|(b?)\\.{,2}", "xa"),
    ("^(a*)|a*[^ab]{0,1}a?|a*ac$", "bcx. bbba"),
    ("b|(a)$|a", "ba"),
];

#[test]
fn groups_lie_where_the_c_library_puts_them() {
    let mut differ = Vec::new();
    // with lines told apart the groups are found the other way
    let lines = Options {
        newline: true,
        ..Options::default()
    };
    let both = [Options::default(), lines];
    for (options, (pat, text)) in both.iter().flat_map(|o| AMBIGUOUS.map(|case| (*o, case))) {
        let theirs = Theirs::new(pat.as_bytes(), options);
        let ours = Regex::with_options(pat.as_bytes(), options).unwrap();
        let expected = theirs.spans(text.as_bytes());
        let expected = expected.map(|spans| spans[..=ours.groups()].to_vec());
        let found = ours.captures(text.as_bytes()).map(|found| {
            let span = |i| found.get(i).map(|r| (r.start, r.end));
            (0..=ours.groups()).map(span).collect::<Vec<_>>()
        });
        if found != expected {
            differ.push(format!("{pat:?} on {text:?}: {found:?} not {expected:?}"));
        }
    }
    assert!(differ.is_empty(), "{}", differ.join("\n"));
}
