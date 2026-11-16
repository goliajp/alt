//! Patterns and texts of any bytes: whatever they hold, compiling and
//! searching answer and the three searches agree.

use alt_regex::{Options, Regex, Syntax};

struct Rng(u64);

impl Rng {
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n as u64) as usize
    }

    fn pieces(&mut self, of: &[&[u8]], most: usize) -> Vec<u8> {
        let count = self.below(most);
        let mut out = Vec::new();
        for _ in 0..count {
            out.extend(of[self.below(of.len())]);
        }
        out
    }
}

/// Operators, characters of every length, and bytes that are none.
const PATTERN: [&[u8]; 40] = [
    b"a",
    b"b",
    b"I",
    b"s",
    b"(",
    b")",
    b"|",
    b"*",
    b"+",
    b"?",
    b"{1,2}",
    b"{2,}",
    b"\\(",
    b"\\)",
    b"\\|",
    b"\\{2\\}",
    b".",
    b"^",
    b"$",
    b"\\b",
    b"\\B",
    b"\\<",
    b"\\>",
    b"\\w",
    b"\\W",
    b"\\1",
    b"\\2",
    b"[",
    b"]",
    b"[^",
    b"-",
    b"[:alpha:]",
    b"[=a=]",
    b"\xe9",
    b"\xa9",
    b"\xc3\xa9",
    b"\xc4\xb1",
    b"\xe2\x82\xac",
    b"\xfd\xbf\xbf\xbf\xbf\xbf",
    b"\xf8",
];
const TEXT: [&[u8]; 16] = [
    b"a",
    b"b",
    b"A",
    b"i",
    b" ",
    b"\n",
    b"_",
    b"\xe9",
    b"\xa9",
    b"\xbf",
    b"\xc3\xa9",
    b"\xc4\xb1",
    b"\xe2\x82\xac",
    b"\xf8\x88\x80\x80\xaa",
    b"\xfd\xbf\xbf\xbf\xbf\xbf",
    b"\xfc",
];

fn run(seed: u64, count: usize) -> usize {
    let mut rng = Rng(seed);
    let mut compiled = 0;
    for turn in 0..count {
        let options = Options {
            syntax: [Syntax::Extended, Syntax::Basic][turn & 1],
            ignore_case: turn & 2 != 0,
            newline: turn & 4 != 0,
            utf8: turn & 8 != 0,
        };
        let pattern = rng.pieces(&PATTERN, 9);
        let Ok(re) = Regex::with_options(&pattern, options) else {
            continue;
        };
        compiled += 1;
        for _ in 0..4 {
            let text = rng.pieces(&TEXT, 8);
            let found = re.captures(&text);
            let shown = || format!("{} on {}", pattern.escape_ascii(), text.escape_ascii());
            assert_eq!(re.is_match(&text), found.is_some(), "{}", shown());
            let whole = found.as_ref().and_then(|found| found.get(0));
            assert_eq!(re.find(&text), whole, "{}", shown());
            for group in 0..=re.groups() {
                let span = found.as_ref().and_then(|found| found.get(group));
                let fine = span.is_none_or(|span| span.start <= span.end && span.end <= text.len());
                assert!(fine, "group {group} of {}", shown());
            }
        }
    }
    compiled
}

#[test]
fn no_pattern_and_no_text_of_any_bytes_stops_a_search() {
    assert!(run(0x9e3779b97f4a7c15, 40_000) > 8_000);
}

#[test]
#[ignore = "four million patterns of any bytes, needs --release"]
fn no_pattern_and_no_text_of_any_bytes_stops_a_search_long() {
    assert!(run(0x2545f4914f6cdd1d, 4_000_000) > 800_000);
}
