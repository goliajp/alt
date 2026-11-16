//! Random patterns and texts, in both syntaxes and in both kinds of
//! text.

use alt_regex::{Options, Regex, Syntax};

use super::GROUPS;
use super::oracle::Oracle;

pub struct Rng(pub u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn pick<'a>(&mut self, of: &[&'a [u8]]) -> &'a [u8] {
        of[self.below(of.len())]
    }
}

const ATOMS: [&[u8]; 5] = [b"a", b"b", b"c", b"ab", b"."];
const SETS: [&[u8]; 9] = [
    b"[ab]",
    b"[^a]",
    b"[a-c]",
    b"[[:alpha:]]",
    b"[^[:space:]b]",
    b"[b-c]",
    b"[^ab]",
    b"[[:space:]x]",
    b"[]a]",
];
const ESCAPES: [&[u8]; 7] = [b"\\w", b"\\W", b"\\s", b" ", b"\\.", b"\\S", b"x"];
/// Characters of two, three and four bytes, with and without case, and
/// a byte that is no character, alone and in sets. A byte that goes on
/// a character is not written alone: where it lies inside a character
/// of the text the C library finds it behind some patterns and not
/// behind others, `.?\xa9` and not `.*\xa9` in `\xc3\xa9`.
const WIDE: [&[u8]; 15] = [
    "é".as_bytes(),
    "É".as_bytes(),
    "ı".as_bytes(),
    "ß".as_bytes(),
    "€".as_bytes(),
    "😀".as_bytes(),
    b"[a-\xff]",
    "[^é]".as_bytes(),
    "[[:upper:]]".as_bytes(),
    "[[:punct:]é]".as_bytes(),
    "[[:alpha:]_]".as_bytes(),
    "[aé€]".as_bytes(),
    b"\xe9",
    b"[a\xe9]",
    b"[\x80-\xff]",
];
const EDGES: [&[u8]; 6] = [b"^", b"$", b"\\b", b"\\B", b"\\<", b"\\>"];

/// Writes random patterns in one syntax.
struct Writer<'a> {
    rng: &'a mut Rng,
    basic: bool,
    utf8: bool,
    out: Vec<u8>,
}

impl Writer<'_> {
    /// An operator, which a basic expression writes behind a backslash.
    fn op(&mut self, op: &[u8]) {
        for (i, &b) in op.iter().enumerate() {
            let escaped = matches!(b, b'(' | b')' | b'|' | b'{' | b'}' | b'+' | b'?');
            // of an interval only the braces
            if self.basic && escaped && (i == 0 || b == b'}') {
                self.out.push(b'\\');
            }
            self.out.push(b);
        }
    }

    /// A random expression; says whether it may match nothing. Nothing
    /// that can match the empty string is repeated without bound: the C
    /// library may then never answer, and where it does it can give a
    /// group a turn that the turns before it leave no room for.
    fn expression(&mut self, depth: u32) -> bool {
        let pieces = 1 + self.rng.below(3);
        // whether the branch being written can be empty so far, and
        // whether one before it may
        let mut all_empty = true;
        let mut before = false;
        for _ in 0..pieces {
            let kinds = if depth == 0 { 7 } else { 10 };
            let empty = match self.rng.below(kinds) {
                0..=2 => {
                    let atom = self.rng.pick(&ATOMS);
                    self.out.extend(atom);
                    Some(false)
                }
                3 | 4 => {
                    let set = self.rng.pick(&SETS);
                    self.out.extend(set);
                    Some(false)
                }
                5 => {
                    let escape = self.rng.pick(&ESCAPES);
                    self.out.extend(escape);
                    Some(false)
                }
                6 if self.utf8 => {
                    let wide = self.rng.pick(&WIDE);
                    self.out.extend(wide);
                    Some(false)
                }
                6 => {
                    // a parenthesis that stands for itself
                    self.out.extend(if self.basic { &b"("[..] } else { b"\\(" });
                    Some(false)
                }
                7 | 8 => {
                    // now and then a group of nothing
                    self.op(b"(");
                    let empty = self.rng.below(8) == 0 || self.expression(depth - 1);
                    self.op(b")");
                    Some(empty)
                }
                _ => {
                    // now and then a branch of nothing
                    let bare = self.rng.below(8);
                    let left = bare == 0 || self.expression(depth - 1);
                    self.op(b"|");
                    let right = bare == 1 || self.expression(depth - 1);
                    // the branch ends in the left side, and the right
                    // side begins the next
                    before |= left || right;
                    all_empty = right;
                    continue;
                }
            };
            all_empty &= self.repeat(empty == Some(false)) || empty == Some(true);
        }
        before || all_empty
    }

    /// A repetition of what was just written, without bound only when
    /// that takes something; says whether it may be left out.
    fn repeat(&mut self, takes: bool) -> bool {
        let bounded: [&[u8]; 10] = [
            b"", b"", b"", b"?", b"{2}", b"{1,2}", b"{0,1}", b"{,2}", b"{3}", b"{2,4}",
        ];
        let unbounded: [&[u8]; 4] = [b"*", b"+", b"{2,}", b"{3,}"];
        let repeat = match (takes, self.rng.below(14)) {
            (true, at @ 10..) => unbounded[at - 10],
            (_, at) => bounded[at % 10],
        };
        self.op(repeat);
        matches!(repeat, b"*" | b"?" | b"{0,1}" | b"{,2}")
    }

    /// Atoms that each take something, with the edges of words and
    /// anchors between them. Behind a group or a repetition that can
    /// take nothing the C library does not hold an edge to what it
    /// means: `a*\B` matches `xa` at its end, `a{0,1}\B` not at all.
    fn flat(mut self) -> Vec<u8> {
        let pieces = 2 + self.rng.below(4);
        for piece in 0..=pieces {
            if self.rng.below(2) == 0 {
                let edge = EDGES[self.rng.below(6)];
                self.out.extend(edge);
            }
            if piece == pieces {
                break;
            }
            let atom = match self.rng.below(if self.utf8 { 4 } else { 3 }) {
                0 => self.rng.pick(&ATOMS),
                1 => self.rng.pick(&SETS),
                2 => self.rng.pick(&ESCAPES),
                _ => self.rng.pick(&WIDE),
            };
            self.out.extend(atom);
            let repeats: [&[u8]; 8] = [b"", b"", b"", b"+", b"{2}", b"{1,2}", b"{2,4}", b"{2,}"];
            let repeat = repeats[self.rng.below(8)];
            self.op(repeat);
        }
        self.out
    }

    /// A whole pattern: expressions with, now and then, an anchor
    /// before and behind them; or a flat one. Behind a group that may
    /// be left out the C library does not hold an anchor to what it
    /// means either.
    fn pattern(mut self) -> Vec<u8> {
        if self.rng.below(4) == 0 {
            return self.flat();
        }
        let parts = 1 + self.rng.below(2);
        for part in 0..=parts {
            if self.rng.below(4) == 0 {
                match part {
                    0 => self.out.extend(EDGES[0]),
                    _ if part == parts => self.out.extend(EDGES[1]),
                    _ => {}
                }
            }
            if part < parts {
                self.expression(2);
            }
        }
        self.out
    }
}

fn text(rng: &mut Rng, options: Options) -> Vec<u8> {
    let plain: [&[u8]; 10] = [b"a", b"a", b"b", b"b", b"c", b" ", b"x", b".", b"_", b"("];
    let wide: [&[u8]; 12] = [
        "ı".as_bytes(),
        "ſ".as_bytes(),
        "é".as_bytes(),
        "É".as_bytes(),
        "ß".as_bytes(),
        "€".as_bytes(),
        "😀".as_bytes(),
        "\u{2003}".as_bytes(),
        b"\xe9",
        b"\xa9",
        b"\xc3",
        b"\xed\xa0\x80",
    ];
    let len = rng.below(10);
    let mut out = Vec::new();
    // without regard to case the C library misses a match behind a
    // character whose upper-case form is shorter, as of the dotless i,
    // once a byte that is no character follows: a text has the one kind
    // or the other
    let kinds = match (options.ignore_case, rng.below(2)) {
        (false, _) => 0..wide.len(),
        (true, 0) => 0..8,
        (true, _) => 2..wide.len(),
    };
    for _ in 0..len {
        let kind = rng.below(20);
        out.extend(match kind {
            0..=5 if options.utf8 => wide[kinds.start + rng.below(kinds.len())],
            6 if options.newline => b"\n",
            _ => plain[rng.below(plain.len())],
        });
    }
    out
}

/// What a run of random patterns came to.
#[derive(Default, Debug)]
pub struct Tally {
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
        if self.differences.len() < 60 {
            self.differences.push(what);
        }
    }

    /// Fails with every difference found.
    pub fn settle(self, least: usize) -> Self {
        assert!(self.differences.is_empty(), "{:#?}", self.differences);
        assert!(self.compiled > least, "{self:?}");
        assert!(self.matches > least, "{self:?}");
        assert!(self.unanswered.len() * 1000 < self.compiled, "{self:?}");
        println!("{self:?}");
        self
    }
}

fn shown(bytes: &[u8]) -> String {
    bytes.escape_ascii().to_string()
}

/// A random pattern for texts read as `options` say.
fn written(rng: &mut Rng, options: Options) -> Vec<u8> {
    let writer = Writer {
        rng,
        basic: options.syntax == Syntax::Basic,
        utf8: options.utf8,
        out: Vec::new(),
    };
    writer.pattern()
}

/// Whether a character of `text` begins with the byte the patterns hold
/// as no character: the C library then lets what follows that byte in
/// the pattern take the rest of the character.
fn taken_apart(text: &[u8]) -> bool {
    text.windows(3).any(|w| w == b"\xe9\xa9\xa9")
}

/// Runs `count` random patterns. Whether a pattern compiles, whether a
/// text matches and where the match and each of its groups lie must
/// agree.
pub fn compare(seed: u64, count: usize, options: Options) -> Tally {
    let mut rng = Rng(seed);
    let mut tally = Tally::default();
    let mut oracle = Oracle::start(options.utf8);
    for _ in 0..count {
        let pat = written(&mut rng, options);
        let texts: Vec<Vec<u8>> = (0..6).map(|_| text(&mut rng, options)).collect();
        let Some(theirs) = oracle.ask((pat.clone(), options, texts.clone())) else {
            tally.unanswered.push(shown(&pat));
            continue;
        };
        let ours = Regex::with_options(&pat, options);
        // for a pattern that holds a byte that is no character the C
        // library reports groups no match has, one with an end before
        // its start: `(.*).\xc3+` on `a\xc3\x9fbb`
        let loose = options.utf8 && std::str::from_utf8(&pat).is_err();
        let pat = shown(&pat);
        if ours.is_ok() != theirs.is_some() {
            tally.differs(format!("pattern {pat}: ours {:?}", ours.as_ref().err()));
            continue;
        }
        let (Ok(ours), Some(theirs)) = (ours, theirs) else {
            continue;
        };
        tally.compiled += 1;
        for (text, expected) in texts.iter().zip(theirs) {
            if loose && taken_apart(text) {
                continue;
            }
            let shown = shown(text);
            let wanted = expected.as_ref().and_then(|spans| spans[0]);
            if ours.is_match(text) != expected.is_some() {
                tally.differs(format!("{pat} on {shown}: theirs {wanted:?}"));
                continue;
            }
            let found = ours.captures(text);
            let span = |i| {
                found
                    .as_ref()
                    .and_then(|c| c.get(i))
                    .map(|r| (r.start, r.end))
            };
            let got = span(0);
            if got != wanted {
                tally.differs(format!("{pat} on {shown}: ours {got:?} theirs {wanted:?}"));
                continue;
            }
            let located = ours.find(text).map(|r| (r.start, r.end));
            if located != got {
                tally.differs(format!("find of {pat} on {shown}: {located:?} not {got:?}"));
            }
            if let Some(expected) = expected.filter(|_| !loose) {
                let last = ours.groups().min(GROUPS - 1);
                let ours: Vec<_> = (0..=last).map(span).collect();
                tally.matches += 1;
                if ours != expected[..=last] {
                    let theirs = &expected[..=last];
                    tally.differs(format!("{pat} on {shown}: ours {ours:?} theirs {theirs:?}"));
                }
            }
        }
    }
    tally
}

/// The four ways a pattern is read, for texts of bytes or of characters.
fn readings(utf8: bool) -> [Options; 4] {
    let plain = Options {
        utf8,
        ..Options::default()
    };
    [
        plain,
        Options {
            ignore_case: true,
            ..plain
        },
        Options {
            newline: true,
            ..plain
        },
        Options {
            syntax: Syntax::Basic,
            ..plain
        },
    ]
}

#[test]
fn random_expressions_match_as_the_c_library_does() {
    let seeds = [
        0x9e3779b97f4a7c15,
        0x2545f4914f6cdd1d,
        0xd1342543de82ef95,
        0x94d049bb133111eb,
    ];
    for (seed, options) in seeds.into_iter().zip(readings(false)) {
        compare(seed, 15_000, options).settle(5_000);
    }
}

#[test]
fn random_expressions_match_utf8_texts_as_the_c_library_does() {
    let seeds = [
        0xbf58476d1ce4e5b9,
        0xda942042e4dd58b5,
        0x853c49e6748fea9b,
        0x5851f42d4c957f2d,
    ];
    for (seed, options) in seeds.into_iter().zip(readings(true)) {
        compare(seed, 5_000, options).settle(1_500);
    }
}

#[test]
#[ignore = "four million random patterns, needs --release"]
fn random_expressions_match_as_the_c_library_does_long() {
    for utf8 in [false, true] {
        for (turn, options) in readings(utf8).into_iter().enumerate() {
            let seed = (turn as u64 + 1 + 4 * u64::from(utf8)).wrapping_mul(0x9e3779b97f4a7c15) | 1;
            compare(seed, 500_000, options).settle(200_000);
        }
    }
}
