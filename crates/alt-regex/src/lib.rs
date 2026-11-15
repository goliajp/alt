//! POSIX regular expressions over byte strings.
//!
//! Both the basic and the extended syntax are read, with the operators
//! the GNU C library adds to them, except back-references: with those a
//! pattern is no longer regular. A match is the one `regexec` reports,
//! the earliest and of those the longest, and it is found in time linear
//! in the length of the text however the pattern is built.
//!
//! Texts are bytes. A multi-byte character is the bytes it is made of,
//! and letters outside ASCII have no case.

mod backtrack;
mod class;
mod dfa;
mod literal;
mod pike;
mod program;
mod syntax;

use std::ops::Range;
use std::sync::Mutex;

use alt_bytes::ByteSlice;

pub use class::ByteSet;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Syntax {
    /// POSIX extended regular expressions.
    #[default]
    Extended,
    /// POSIX basic regular expressions.
    Basic,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Options {
    pub syntax: Syntax,
    /// ASCII letters match in either case.
    pub ignore_case: bool,
    /// `^` and `$` also match around a newline inside the text, and
    /// neither `.` nor a negated bracket expression matches one.
    pub newline: bool,
}

/// Why a pattern was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
    /// A parenthesis without its partner.
    Paren,
    /// A bracket expression without its end.
    Bracket,
    /// A malformed `{m,n}`.
    Brace,
    /// A repetition with nothing to repeat.
    Repeat,
    /// A backslash at the end of the pattern.
    Escape,
    /// A range that ends before it begins.
    Range,
    /// An unknown character class name.
    Class,
    /// A collating element of more than one byte.
    Collate,
    /// A back-reference, which this matcher does not have.
    BackReference,
    /// A pattern too large to compile.
    TooBig,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Error::Paren => "unmatched parenthesis",
            Error::Bracket => "unterminated bracket expression",
            Error::Brace => "invalid repetition count",
            Error::Repeat => "repetition with nothing to repeat",
            Error::Escape => "trailing backslash",
            Error::Range => "invalid range end",
            Error::Class => "invalid character class name",
            Error::Collate => "invalid collating element",
            Error::BackReference => "back-references are not supported",
            Error::TooBig => "regular expression too big",
        })
    }
}

impl std::error::Error for Error {}

/// Storage the searches with one pattern reuse. A thread that searches
/// often keeps its own and passes it to the `_with` methods; the others
/// share one inside the pattern, behind a lock.
pub struct Cache {
    /// for a match that begins anywhere
    scan: dfa::Cache,
    /// for a match that begins where the search does
    fixed: dfa::Cache,
    pike: pike::Cache,
    backtrack: backtrack::Cache,
}

impl Default for Cache {
    fn default() -> Self {
        Cache {
            scan: dfa::Cache::new(true),
            fixed: dfa::Cache::new(false),
            pike: pike::Cache::default(),
            backtrack: backtrack::Cache::default(),
        }
    }
}

/// A compiled pattern.
pub struct Regex {
    prog: program::Program,
    /// Bytes every match contains.
    literal: Option<Vec<u8>>,
    /// The first capture slots of the match in the empty text, if it has
    /// one.
    on_empty: Option<[usize; FEW]>,
    cache: Mutex<Cache>,
}

/// Where a match and its groups lie in the text.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Captures {
    slots: Slots,
}

/// The slots of a few groups lie in the value itself.
#[derive(Clone, PartialEq, Eq, Debug)]
enum Slots {
    Few([usize; FEW]),
    Many(Vec<usize>),
}

const FEW: usize = 8;

impl Captures {
    /// The span of group `index`; group 0 is the whole match. `None` for
    /// a group that took no part in the match.
    pub fn get(&self, index: usize) -> Option<Range<usize>> {
        let slots = match &self.slots {
            Slots::Few(slots) => slots.as_slice(),
            Slots::Many(slots) => slots.as_slice(),
        };
        let from = *slots.get(2 * index)?;
        let to = *slots.get(2 * index + 1)?;
        (from != pike::UNSET && to != pike::UNSET).then_some(from..to)
    }
}

impl Regex {
    /// Compiles an extended regular expression.
    pub fn new(pattern: &[u8]) -> Result<Self, Error> {
        Self::with_options(pattern, Options::default())
    }

    pub fn with_options(pattern: &[u8], options: Options) -> Result<Self, Error> {
        let (node, groups) = syntax::parse(pattern, options)?;
        let prog = program::compile(&node, groups)?;
        let anchored = prog.anchored;
        // a text with nothing in it is common, and always answered alike
        let mut slots = [pike::UNSET; FEW];
        let width = FEW.min(2 * (groups as usize + 1));
        let cache = &mut pike::Cache::default();
        let matched = pike::search(&prog, cache, b"", None, &mut slots[..width]);
        Ok(Regex {
            prog,
            on_empty: matched.then_some(slots),
            // a pattern tied to the start of the text is refused within a
            // few bytes anyway; looking through the text first costs more
            literal: literal::required(&node).filter(|_| !anchored),
            cache: Mutex::default(),
        })
    }

    /// The number of groups in the pattern.
    pub fn groups(&self) -> usize {
        self.prog.groups as usize
    }

    fn excluded(&self, text: &[u8]) -> bool {
        self.literal
            .as_ref()
            .is_some_and(|literal| text.find(literal).is_none())
    }

    fn shared(&self) -> std::sync::MutexGuard<'_, Cache> {
        self.cache.lock().expect("a search does not panic")
    }

    /// Whether the pattern matches anywhere in `text`.
    pub fn is_match(&self, text: &[u8]) -> bool {
        !self.excluded(text) && self.decide(&mut self.shared(), text)
    }

    /// As [`Regex::is_match`], with the caller's storage.
    pub fn is_match_with(&self, cache: &mut Cache, text: &[u8]) -> bool {
        !self.excluded(text) && self.decide(cache, text)
    }

    fn decide(&self, cache: &mut Cache, text: &[u8]) -> bool {
        if text.is_empty() {
            return self.on_empty.is_some();
        }
        if self.prog.plain_looks {
            return dfa::earliest_end(&self.prog, &mut cache.scan, text).is_some();
        }
        pike::search(
            &self.prog,
            &mut cache.pike,
            text,
            None,
            &mut [pike::UNSET; 2],
        )
    }

    /// Where the earliest match begins and where the longest one from
    /// there ends, as far as the tables could tell; `None` when there is
    /// no match. The text is one the tables can search.
    fn locate(&self, cache: &mut Cache, text: &[u8]) -> Option<(Option<usize>, Option<usize>)> {
        // trying each start costs what the text is long in the usual
        // case; a pattern for which it costs more is left to the slower,
        // steady search
        let mut work = 8 * text.len() + 64;
        if self.prog.anchored {
            let end = dfa::longest(&self.prog, &mut cache.fixed, text, 0, &mut work)?;
            return Some((Some(0), Some(end)));
        }
        // a match ends by here, so the earliest one begins by here
        let ends = dfa::earliest_end(&self.prog, &mut cache.scan, text)?;
        for from in 0..=ends {
            if let Some(end) = dfa::longest(&self.prog, &mut cache.fixed, text, from, &mut work) {
                return Some((Some(from), Some(end)));
            }
            if work == 0 {
                break;
            }
        }
        Some((None, None))
    }

    /// Fills `slots` with the capture slots of the earliest, longest
    /// match; false when there is none.
    fn search(&self, cache: &mut Cache, text: &[u8], slots: &mut [usize]) -> bool {
        if text.is_empty() {
            let Some(known) = &self.on_empty else {
                return false;
            };
            if slots.len() <= FEW {
                slots.copy_from_slice(&known[..slots.len()]);
                return true;
            }
            return backtrack::captures(&self.prog, &mut cache.backtrack, text, 0, 0, slots);
        }
        if self.excluded(text) {
            return false;
        }
        let mut from = None;
        if self.prog.plain_looks {
            // most texts match nothing, which the tables tell fastest;
            // and where a match lies, which leaves only its groups
            let Some((begins, ends)) = self.locate(cache, text) else {
                return false;
            };
            if let (Some(begins), Some(ends)) = (begins, ends) {
                if slots.len() == 2 {
                    slots.copy_from_slice(&[begins, ends]);
                    return true;
                }
                if backtrack::fits(&self.prog, ends - begins) {
                    let prog = &self.prog;
                    return backtrack::captures(
                        prog,
                        &mut cache.backtrack,
                        text,
                        begins,
                        ends,
                        slots,
                    );
                }
            }
            from = begins;
        }
        pike::search(&self.prog, &mut cache.pike, text, from, slots)
    }

    /// The earliest match in `text`, and of those the longest.
    pub fn find(&self, text: &[u8]) -> Option<Range<usize>> {
        self.find_with(&mut self.shared(), text)
    }

    /// As [`Regex::find`], with the caller's storage.
    pub fn find_with(&self, cache: &mut Cache, text: &[u8]) -> Option<Range<usize>> {
        let mut slots = [pike::UNSET; 2];
        self.search(cache, text, &mut slots)
            .then(|| slots[0]..slots[1])
    }

    /// The earliest, longest match with the spans of all its groups.
    pub fn captures(&self, text: &[u8]) -> Option<Captures> {
        self.captures_upto(text, self.groups())
    }

    /// As [`Regex::captures`], recording only the groups up to number
    /// `last`: fewer groups to carry make a faster search.
    pub fn captures_upto(&self, text: &[u8], last: usize) -> Option<Captures> {
        self.captures_upto_with(&mut self.shared(), text, last)
    }

    /// As [`Regex::captures_upto`], with the caller's storage.
    pub fn captures_upto_with(
        &self,
        cache: &mut Cache,
        text: &[u8],
        last: usize,
    ) -> Option<Captures> {
        let width = 2 * (last.min(self.groups()) + 1);
        if width <= FEW {
            let mut few = [pike::UNSET; FEW];
            let found = self.search(cache, text, &mut few[..width]);
            return found.then_some(Captures {
                slots: Slots::Few(few),
            });
        }
        let mut slots = vec![pike::UNSET; width];
        let found = self.search(cache, text, &mut slots);
        found.then_some(Captures {
            slots: Slots::Many(slots),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spans(pattern: &str, text: &str) -> Option<Vec<Option<Range<usize>>>> {
        let re = Regex::new(pattern.as_bytes()).unwrap();
        let found = re.captures(text.as_bytes())?;
        assert!(re.is_match(text.as_bytes()), "{pattern} on {text}");
        Some((0..=re.groups()).map(|i| found.get(i)).collect())
    }

    #[test]
    fn the_match_is_the_earliest_and_of_those_the_longest() {
        assert_eq!(spans("b+", "abbbc"), Some(vec![Some(1..4)]));
        assert_eq!(spans("a|ab|abc", "xabcd"), Some(vec![Some(1..4)]));
        assert_eq!(spans("bc|abcd?", "abcd"), Some(vec![Some(0..4)]));
        assert_eq!(spans("x*", "aaxx"), Some(vec![Some(0..0)]));
        assert_eq!(spans("q", "abc"), None);
        assert_eq!(spans("", ""), Some(vec![Some(0..0)]));
    }

    #[test]
    fn groups_report_where_they_last_matched_or_nothing() {
        let found = spans("^[ \t]*((class|def) +([a-z]+))", "  def run(self):").unwrap();
        assert_eq!(found, vec![Some(0..9), Some(2..9), Some(2..5), Some(6..9)]);
        let found = spans("(a)|(b)", "b").unwrap();
        assert_eq!(found, vec![Some(0..1), None, Some(0..1)]);
        let found = spans("(a|b)+", "abab").unwrap();
        assert_eq!(found, vec![Some(0..4), Some(3..4)]);
    }

    #[test]
    fn anchors_and_word_edges_hold_only_where_they_should() {
        assert!(spans("^abc$", "abc").is_some());
        assert!(spans("^abc$", "abcd").is_none());
        assert!(spans("^abc$", "xabc").is_none());
        assert!(spans("^$", "").is_some());
        assert_eq!(spans("\\bcat\\b", "a cat."), Some(vec![Some(2..5)]));
        assert!(spans("\\bcat\\b", "concat").is_none());
        assert_eq!(spans("\\<at", "cat at"), Some(vec![Some(4..6)]));
        assert_eq!(spans("\\w+\\>", "ab cd"), Some(vec![Some(0..2)]));
        let lines = Options {
            newline: true,
            ..Options::default()
        };
        let re = Regex::with_options(b"^b.$", lines).unwrap();
        assert_eq!(re.find(b"ab\nbc\nb\n"), Some(3..5));
    }

    #[test]
    fn counted_and_nested_repetitions_match_what_they_count() {
        assert_eq!(spans("a{2,3}", "aaaa"), Some(vec![Some(0..3)]));
        assert!(spans("^a{2,3}$", "aaaa").is_none());
        assert!(spans("^(ab){2}$", "abab").is_some());
        assert!(spans("^(ab){0}c$", "c").is_some());
        assert_eq!(spans("(a*)*b", "aab"), Some(vec![Some(0..3), Some(0..2)]));
        assert!(spans("^(a|b|ab)*c$", "ababbac").is_some());
    }

    #[test]
    fn ignoring_case_and_the_basic_syntax_are_options() {
        let any_case = Options {
            ignore_case: true,
            ..Options::default()
        };
        let re = Regex::with_options(b"^procedure [a-z]+", any_case).unwrap();
        assert!(re.is_match(b"PROCEDURE Main"));
        let basic = Options {
            syntax: Syntax::Basic,
            ..Options::default()
        };
        let re = Regex::with_options(b"^\\(a\\{2\\}\\)b+$", basic).unwrap();
        assert_eq!(re.captures(b"aab+").unwrap().get(1), Some(0..2));
        assert!(!re.is_match(b"aabb"));
    }

    #[test]
    fn both_ways_of_finding_the_groups_agree() {
        for (pattern, text) in [
            ("(a|ab)(c|bcd)(d*)", "abcd"),
            ("(a*)*(b|ab)", "aaab"),
            ("((x?)y?)*z", "xyyxz"),
            ("^(.*)(.*)$", "hello"),
            ("(a{1,2}){2,3}(a*)", "aaaaa"),
            ("(()|a)+b", "aab"),
        ] {
            let re = Regex::new(pattern.as_bytes()).unwrap();
            let text = text.as_bytes();
            let mut by_steps = vec![pike::UNSET; 2 * (re.groups() + 1)];
            let stepped =
                pike::search(&re.prog, &mut Default::default(), text, None, &mut by_steps);
            assert!(stepped, "{pattern}");
            let mut by_paths = vec![pike::UNSET; by_steps.len()];
            let (from, to) = (by_steps[0], by_steps[1]);
            let cache = &mut backtrack::Cache::default();
            assert!(backtrack::captures(
                &re.prog,
                cache,
                text,
                from,
                to,
                &mut by_paths
            ));
            assert_eq!(by_paths, by_steps, "{pattern}");
        }
    }

    #[test]
    fn find_gives_the_whole_match_of_captures() {
        for (pattern, text) in [
            ("(b+)c?", "abbbcd"),
            ("x*(y|z)", "aaxxz"),
            ("(a|ab)(c|bcd)?", "zabcd"),
            ("(q)", "abc"),
            ("(a*)$", "baa"),
            ("^(a*)", "aab"),
            ("(a+)+$", "aaab aaa"),
            ("\\<(b)", "ab b"),
        ] {
            let re = Regex::new(pattern.as_bytes()).unwrap();
            let whole = re.captures(text.as_bytes()).and_then(|c| c.get(0));
            assert_eq!(re.find(text.as_bytes()), whole, "{pattern} on {text}");
            let upto = re.captures_upto(text.as_bytes(), 0).and_then(|c| c.get(0));
            assert_eq!(upto, whole, "{pattern} on {text}");
        }
        let re = Regex::new(b"(a|ab)(c|bcd)?").unwrap();
        assert_eq!(re.find(b"zabcd"), Some(1..5));
    }

    #[test]
    fn a_text_without_the_required_bytes_is_refused_before_matching() {
        let re = Regex::new(b"[ \t]*static [a-z]+").unwrap();
        assert_eq!(re.literal.as_deref(), Some(&b"static "[..]));
        assert!(!re.is_match(b"int main(void)"));
        assert!(re.is_match(b"\tstatic int x"));
        assert!(Regex::new(b"^static [a-z]+").unwrap().literal.is_none());
    }
}
