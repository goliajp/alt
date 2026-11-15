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

/// What a run of random patterns came to.
#[derive(Default, Debug)]
struct Tally {
    compiled: usize,
    matches: usize,
    /// matches whose groups all lie where the C library puts them
    same_groups: usize,
    /// where the two disagree on a pattern, on a match or on its span
    differences: Vec<String>,
    /// a few of the matches whose groups lie elsewhere
    other_groups: Vec<String>,
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
        println!("{self:?}");
        self
    }
}

/// Runs `count` random patterns. Whether a pattern compiles, whether a
/// text matches and where the whole match lies must agree; where the
/// groups lie is counted.
fn compare(seed: u64, count: usize, options: Options) -> Tally {
    let mut rng = Rng(seed);
    let mut tally = Tally::default();
    for _ in 0..count {
        // the anchors stand at the ends of the pattern, and there are no
        // word edges: inside a repeated group the C library loses track
        // of either
        let mut pat = String::from(rng.pick(&["", "", "", "^"]));
        let _ = pattern(&mut rng, 2, &mut pat);
        pat.push_str(rng.pick(&["", "", "", "$"]));
        let theirs = Theirs::new(pat.as_bytes(), options);
        let ours = Regex::with_options(pat.as_bytes(), options);
        if ours.is_ok() != theirs.0.is_some() {
            tally.differs(format!("pattern {pat:?}: ours {:?}", ours.as_ref().err()));
            continue;
        }
        let Ok(ours) = ours else { continue };
        tally.compiled += 1;
        for _ in 0..6 {
            let text = text(&mut rng);
            let shown = String::from_utf8_lossy(&text);
            let expected = theirs.spans(&text);
            let wanted = expected.as_ref().and_then(|spans| spans[0]);
            if ours.is_match(&text) != expected.is_some() {
                tally.differs(format!("{pat:?} on {shown:?}: theirs {wanted:?}"));
                continue;
            }
            let found = ours.captures(&text);
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
            let located = ours.find(&text).map(|r| (r.start, r.end));
            if located != got {
                tally.differs(format!(
                    "find of {pat:?} on {shown:?}: {located:?} not {got:?}"
                ));
            }
            if let (Some(found), Some(expected)) = (found, expected) {
                let groups = (0..=ours.groups().min(GROUPS - 1))
                    .all(|i| found.get(i).map(|r| (r.start, r.end)) == expected[i]);
                tally.matches += 1;
                tally.same_groups += usize::from(groups);
                if !groups && tally.other_groups.len() < 12 {
                    let ours: Vec<_> = (0..=ours.groups()).map(|i| found.get(i)).collect();
                    let theirs = &expected[..=ours.len() - 1];
                    let example = format!("{pat:?} on {shown:?}: ours {ours:?} theirs {theirs:?}");
                    tally.other_groups.push(example);
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
