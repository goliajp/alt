//! The same patterns and texts given to this crate and to the C
//! library's `regcomp` and `regexec`: both must accept the same patterns
//! and report the same match.

use std::ffi::{CString, c_char, c_int};

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

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn pick<'a>(&mut self, of: &[&'a str]) -> &'a str {
        of[self.below(of.len())]
    }
}

/// A random extended expression; says whether it can match nothing.
/// Nothing that can match the empty string is repeated without bound:
/// the C library may then never answer.
fn pattern(rng: &mut Rng, depth: u32, out: &mut String) -> bool {
    let pieces = 1 + rng.below(3);
    let mut all_empty = true;
    for _ in 0..pieces {
        // whether the piece can be empty; none for a piece no repetition
        // can be written behind
        let empty = match rng.below(if depth == 0 { 6 } else { 9 }) {
            0..=2 => {
                out.push_str(rng.pick(&["a", "b", "c", "ab", "."]));
                Some(false)
            }
            3 => {
                let sets = ["[ab]", "[^a]", "[a-c]", "[[:alpha:]]", "[^[:space:]b]"];
                out.push_str(rng.pick(&sets));
                Some(false)
            }
            4 => {
                out.push_str(rng.pick(&["[b-c]", "[^ab]", "[[:space:]x]", "[]a]"]));
                Some(false)
            }
            5 => {
                out.push_str(rng.pick(&["\\w", "\\W", "\\s", " ", "\\.", "\\(", "x"]));
                Some(false)
            }
            6 | 7 => {
                out.push('(');
                let empty = pattern(rng, depth - 1, out);
                out.push(')');
                Some(empty)
            }
            _ => {
                let left = pattern(rng, depth - 1, out);
                out.push('|');
                let right = pattern(rng, depth - 1, out);
                all_empty &= left || right;
                continue;
            }
        };
        let bounded = ["", "", "", "?", "{2}", "{1,2}", "{0,1}", "{,2}"];
        let unbounded = ["*", "+", "{2,}"];
        let repeat = match (empty, rng.below(11)) {
            (Some(false), 8..) => unbounded[rng.below(3)],
            (_, at) => bounded[at % 8],
        };
        out.push_str(repeat);
        let optional = matches!(repeat, "*" | "?" | "{0,1}" | "{,2}");
        all_empty &= empty == Some(true) || optional;
    }
    all_empty
}

fn text(rng: &mut Rng) -> Vec<u8> {
    let len = rng.below(10);
    (0..len).map(|_| b"aabbc x."[rng.below(8)]).collect()
}

type Spans = Vec<Option<(usize, usize)>>;
type Question = (Vec<u8>, Options, Vec<Vec<u8>>);
/// For each text its spans; `None` for a pattern that is refused.
type Answer = Option<Vec<Option<Spans>>>;

/// The C library on a thread of its own. For some patterns it never
/// answers; it is then left behind and another takes its place.
struct Oracle {
    ask: std::sync::mpsc::Sender<Question>,
    answer: std::sync::mpsc::Receiver<Answer>,
}

impl Oracle {
    fn start() -> Self {
        let (ask, asked) = std::sync::mpsc::channel::<Question>();
        let (answered, answer) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for (pattern, options, texts) in asked {
                let theirs = Theirs::new(&pattern, options);
                let compiled = theirs.0.is_some();
                let reply = compiled.then(|| texts.iter().map(|t| theirs.spans(t)).collect());
                if answered.send(reply).is_err() {
                    break;
                }
            }
        });
        Oracle { ask, answer }
    }

    /// What the C library says, unless it takes longer than a pattern
    /// of this size ever should.
    fn ask(&mut self, question: Question) -> Option<Answer> {
        self.ask
            .send(question)
            .expect("the thread waits for questions");
        let patience = std::time::Duration::from_secs(20);
        let reply = self.answer.recv_timeout(patience).ok();
        if reply.is_none() {
            *self = Oracle::start();
        }
        reply
    }
}

/// What a run of random patterns came to.
#[derive(Default, Debug)]
struct Tally {
    compiled: usize,
    matches: usize,
    /// where the two disagree on a pattern, on a match, on its span or
    /// on the span of a group
    differences: Vec<String>,
    /// patterns the C library did not answer for
    unanswered: Vec<String>,
}

impl Tally {
    fn differs(&mut self, what: String) {
        if self.differences.len() < 40 {
            self.differences.push(what);
        }
    }

    /// Fails with every difference found, and says how the groups fared.
    fn settle(self, least: usize) -> Self {
        assert!(self.differences.is_empty(), "{:#?}", self.differences);
        assert!(self.compiled > least, "{self:?}");
        assert!(self.unanswered.len() * 1000 < self.compiled, "{self:?}");
        println!("{self:?}");
        self
    }
}

/// Runs `count` random patterns. Whether a pattern compiles, whether a
/// text matches and where the match and each of its groups lie must
/// agree.
fn compare(seed: u64, count: usize, options: Options) -> Tally {
    let mut rng = Rng(seed);
    let mut tally = Tally::default();
    let mut oracle = Oracle::start();
    for _ in 0..count {
        // the anchors stand at the ends of the pattern, and there are no
        // word edges: inside a repeated group the C library loses track
        // of either
        let mut pat = String::from(rng.pick(&["", "", "", "^"]));
        let _ = pattern(&mut rng, 2, &mut pat);
        pat.push_str(rng.pick(&["", "", "", "$"]));
        let texts: Vec<Vec<u8>> = (0..6).map(|_| text(&mut rng)).collect();
        let Some(theirs) = oracle.ask((pat.clone().into_bytes(), options, texts.clone())) else {
            tally.unanswered.push(pat);
            continue;
        };
        let ours = Regex::with_options(pat.as_bytes(), options);
        if ours.is_ok() != theirs.is_some() {
            tally.differs(format!("pattern {pat:?}: ours {:?}", ours.as_ref().err()));
            continue;
        }
        let (Ok(ours), Some(theirs)) = (ours, theirs) else {
            continue;
        };
        tally.compiled += 1;
        for (text, expected) in texts.iter().zip(theirs) {
            let shown = String::from_utf8_lossy(text);
            let wanted = expected.as_ref().and_then(|spans| spans[0]);
            if ours.is_match(text) != expected.is_some() {
                tally.differs(format!("{pat:?} on {shown:?}: theirs {wanted:?}"));
                continue;
            }
            let found = ours.captures(text);
            let got = found
                .as_ref()
                .and_then(|c| c.get(0))
                .map(|r| (r.start, r.end));
            if got != wanted {
                tally.differs(format!(
                    "{pat:?} on {shown:?}: ours {got:?} theirs {wanted:?}"
                ));
                continue;
            }
            let located = ours.find(text).map(|r| (r.start, r.end));
            if located != got {
                tally.differs(format!(
                    "find of {pat:?} on {shown:?}: {located:?} not {got:?}"
                ));
            }
            if let (Some(found), Some(expected)) = (found, expected) {
                let groups = (0..=ours.groups().min(GROUPS - 1))
                    .all(|i| found.get(i).map(|r| (r.start, r.end)) == expected[i]);
                tally.matches += 1;
                if !groups {
                    let ours: Vec<_> = (0..=ours.groups()).map(|i| found.get(i)).collect();
                    let theirs = &expected[..=ours.len() - 1];
                    tally.differs(format!(
                        "{pat:?} on {shown:?}: ours {ours:?} theirs {theirs:?}"
                    ));
                }
            }
        }
    }
    tally
}

#[test]
fn random_extended_expressions_match_as_the_c_library_does() {
    compare(0x9e3779b97f4a7c15, 30_000, Options::default()).settle(15_000);
    let any_case = Options {
        ignore_case: true,
        ..Options::default()
    };
    compare(0x2545f4914f6cdd1d, 10_000, any_case).settle(5_000);
    let lines = Options {
        newline: true,
        ..Options::default()
    };
    compare(0xd1342543de82ef95, 10_000, lines).settle(5_000);
}

#[test]
#[ignore = "three million random patterns, needs --release"]
fn random_extended_expressions_match_as_the_c_library_does_long() {
    for seed in 1..=3u64 {
        let seed = seed.wrapping_mul(0x9e3779b97f4a7c15) | 1;
        compare(seed, 1_000_000, Options::default()).settle(500_000);
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
