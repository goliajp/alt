//! POSIX regular expressions over byte strings.
//!
//! Both the basic and the extended syntax are read, with the operators
//! the GNU C library adds to them. A match is the one `regexec` reports,
//! the earliest and of those the longest, and it is found in time linear
//! in the length of the text however the pattern is built.
//!
//! The GNU C library is the reference on every system: which patterns
//! are refused, what matches and where each group lies are what its
//! `regcomp` and `regexec` give, also where the C library of the system
//! at hand gives something else. The tests that compare the two are
//! therefore built only where the GNU C library is the C library.
//!
//! Where the groups lie is the C library's answer also where that is not
//! what POSIX says of subexpressions. Once the two ends of a match are
//! known the C library walks one path between them: at each choice it
//! takes the first way, in the order written, that still leads to the
//! end, and behind a turn of a repetition that took nothing it leaves
//! the repetition. So `(b?|a)*a?` on `ba` gives the group the `b`,
//! though the repetition could have taken the `a` too, and `((b*)|a)*`
//! on `a` reports the second group, of a branch the last turn did not
//! take, as empty at 0.
//!
//! A pattern with a back-reference is no longer regular. It is matched
//! by trying its paths one after another, up to a fixed number of steps;
//! a search that would take more is given up.
//!
//! Texts are bytes, as in the locale `C`: a multi-byte character is the
//! bytes it is made of, and letters outside ASCII have no case. With
//! [`Options::utf8`] a text is UTF-8 and is matched as in the locale
//! `C.UTF-8` of the GNU C library 2.41: `.`, a bracket expression, a
//! class, `\w` and the edges of words take whole characters, and case
//! is ignored by reading pattern and text in upper case. The classes and
//! the upper-case forms are those of Unicode 16.0.0. That locale orders
//! no character outside ASCII, so a range that ends in one is refused.
//! Its characters go on past Unicode, to forms of six bytes.
//!
//! A text need not be UTF-8 throughout. A byte of the text that is part
//! of no character is matched by the same byte written in the pattern,
//! where that byte is part of no character either, and by nothing else:
//! not by `.`, a class or a negated bracket expression. Beside the edge
//! of a word it counts as the character of its value. Written in a
//! bracket expression such a byte is no member, and as the end of a
//! range it stands for the character of its value. A pattern of nothing
//! but characters, `.`, anchors, groups, repetitions and bracket
//! expressions that list characters of ASCII one by one, read with
//! regard to case, is matched byte by byte, as the C library matches
//! it: there `.` takes whatever is shaped like a character, and a match
//! may begin inside one.
//!
//! # Where the GNU C library answers otherwise
//!
//! In these cases the C library goes against the meaning of its own
//! operators or reports what no match has, and this crate does not
//! follow it. Each is given with a pattern and a text; the tests hold
//! both sides to what is said here.
//!
//! - An anchor or the edge of a word inside or behind something
//!   repeated or left out. `a*\B` on `xa`: the C library gives the empty
//!   match at 2, where a word ends; here it is at 1. `(\ba){2,}` matches
//!   `aaa` there and nothing here; `(^a?){1,2}` matches `a` here and
//!   nothing there.
//! - A group that is repeated, may match nothing and is referred back
//!   to. `(a*)*\1` on `aaa`: the C library reports a group that ends
//!   before it begins; here it is the empty one at 3.
//! - A back-reference in a pattern that repeats without bound what may
//!   match nothing. To find the groups the C library walks one path
//!   and, come back to where it stood with nothing taken, ends the
//!   walk there: the groups behind that place are reported as taking
//!   no part. `x?*(a)\1` on `aa`: the match and no first group there;
//!   here the group is the first `a`. Of two paths that a match has,
//!   the two may also keep different ones.
//! - Without [`Options::newline`], an anchor beside a newline in the
//!   text. Where the C library steps from byte to byte it takes a
//!   newline for the end of a line whether or not it was asked to, and
//!   where it looks at the text it does not: `a$.` on `a`, a newline
//!   and `b` matches the first two bytes there, `a$` and `(a$).`
//!   nothing. Here such an anchor holds at the ends of the text only.
//! - Without regard to case, a text with a character whose upper-case
//!   form has fewer bytes (the dotless i, the long s) and, later, a
//!   byte that is no character. `x` on the long s, `x` and the byte
//!   `f0`: no match there, the `x` here.
//! - A byte of the pattern that is part of no character, met in the
//!   text where a character begins with it. `\xf0[^a]` on a character
//!   of four bytes that begins with `f0`: the C library lets the bracket
//!   expression take the rest of that character; here nothing takes
//!   part of a character, and there is no match.
//! - Such a byte in the pattern, and the groups: `(.*).\xc3+` on `a`
//!   and the two bytes `c3 9f`. The C library reports a group with a
//!   start and no end; here it is the empty one at 0.
//! - `.*` before a byte that goes on a character: `.*\xa9` on the two
//!   bytes `c3 a9` matches the second byte here, as `.?\xa9` does in
//!   both, and nothing there.
//!
//! What is bounded here and not there, so that a pattern git takes may
//! be refused here: a pattern of more than 262144 bytes, of more than
//! 32767 groups, with groups and repetitions nested more than 400
//! deep, that comes to more than 400000 instructions, or whose tree
//! and program together would hold more than 24 MiB is refused with
//! [`Error::TooBig`]; no pattern holds more than some 32 MiB while it
//! is compiled. The depth is what the stack of a thread holds, and the
//! length is near what the instructions allow a pattern of plain
//! characters.
//!
//! A search holds no more than some 48 MiB however long the text, and
//! most hold a few kilobytes: the tables of states are begun again past
//! 2 MiB each, the paths stepped together carry at most 8 MiB of capture
//! slots at one position of the text and as much at the next, and what
//! a path leaves behind it to come back to is bounded by the size of
//! the program. For each byte of the text a search takes no more steps
//! than the program has instructions, which is at most 400000, and
//! copies no more than a million capture slots; for a pattern that
//! repeats without bound what may match nothing it takes up to 65536
//! steps more, and past those the groups inside such a repetition are
//! those of fewer turns than the C library takes.
//!
//! A search that comes to one of these bounds is given up, and reported
//! as no match by all but [`Regex::try_captures_upto_with`], which says
//! [`Error::Exhausted`]: the C library's `regexec` says `REG_ESPACE`
//! when it has no memory left. That is a search for more groups than
//! the paths from one start of a match carry between them, a thousand
//! groups in a thousand paths; whether and where such a pattern matches
//! and its first groups are found all the same. And it is a search with
//! back-references that takes more than some four million steps or
//! leaves more than a million choices open at once. For a few
//! patterns that repeat without bound what may match nothing the C
//! library never answers; this crate always does.
//!
//! The classes, the upper-case forms and the order of characters are
//! those of the one locale named above, whatever locale the process is
//! in: the Turkish pairing of the dotted and dotless i, or an order of
//! letters that makes `[a-é]` a range, is not to be had.

mod analysis;
mod backref;
mod backtrack;
mod block;
mod bracket;
mod budget;
mod captures;
mod chars;
mod class;
mod dfa;
mod error;
mod literal;
mod pike;
mod program;
mod registers;
mod search;
mod syntax;
mod tables;
mod utf8;

use std::ops::Range;
use std::sync::Mutex;

use captures::FEW;

pub use captures::Captures;
pub use class::ByteSet;
pub use error::Error;

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
    /// The text is UTF-8 and is matched character by character, as the
    /// C library does in a UTF-8 locale; otherwise byte by byte.
    pub utf8: bool,
}

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

impl Regex {
    /// Compiles an extended regular expression.
    pub fn new(pattern: &[u8]) -> Result<Self, Error> {
        Self::with_options(pattern, Options::default())
    }

    pub fn with_options(pattern: &[u8], options: Options) -> Result<Self, Error> {
        let (node, groups, shape) = syntax::parse_shaped(pattern, options)?;
        let prog = program::compile(&node, groups, options, shape)?;
        // a pattern tied to the start of the text is refused within a
        // few bytes anyway; looking through the text first costs more
        let literal = literal::required(&node).filter(|_| !prog.anchored);
        drop(node);
        Ok(Regex {
            on_empty: search::on_empty(&prog),
            prog,
            literal,
            cache: Mutex::default(),
        })
    }

    /// The number of groups in the pattern.
    pub fn groups(&self) -> usize {
        self.prog.groups as usize
    }

    fn shared(&self) -> std::sync::MutexGuard<'_, Cache> {
        self.cache.lock().expect("a search does not panic")
    }

    /// Whether the pattern matches anywhere in `text`.
    pub fn is_match(&self, text: &[u8]) -> bool {
        !self.excluded(text) && self.decide(&mut self.shared(), text) == Some(true)
    }

    /// As [`Regex::is_match`], with the caller's storage.
    pub fn is_match_with(&self, cache: &mut Cache, text: &[u8]) -> bool {
        !self.excluded(text) && self.decide(cache, text) == Some(true)
    }

    /// The earliest match in `text`, and of those the longest.
    pub fn find(&self, text: &[u8]) -> Option<Range<usize>> {
        self.find_with(&mut self.shared(), text)
    }

    /// As [`Regex::find`], with the caller's storage.
    pub fn find_with(&self, cache: &mut Cache, text: &[u8]) -> Option<Range<usize>> {
        let mut slots = [pike::UNSET; 2];
        let found = self.search(cache, text, &mut slots) == Some(true);
        found.then(|| slots[0]..slots[1])
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
        // a first branch of nothing is tried behind the second
        assert_eq!(spans("|(b?)", ""), Some(vec![Some(0..0), Some(0..0)]));
        assert_eq!(spans("(b?)|", ""), Some(vec![Some(0..0), Some(0..0)]));
        let found = spans("(|a)+(a*b)", "aaab").unwrap();
        assert_eq!(found, vec![Some(0..4), Some(2..3), Some(3..4)]);
        let found = spans("(()|a)+(a*b)", "aaab").unwrap();
        assert_eq!(found, vec![Some(0..4), Some(0..0), Some(0..0), Some(0..4)]);
        // a repetition of what may match nothing is left behind a turn
        // of nothing, though it could take more
        assert_eq!(spans("(b?|a)*a?", "ba"), Some(vec![Some(0..2), Some(0..1)]));
        let found = spans("((b*)|a)*", "a").unwrap();
        assert_eq!(found, vec![Some(0..1), Some(0..1), Some(0..0)]);
        // the second of the copies that must be there gives way to what
        // the first found, as one that may be left out
        // repetitions written behind one another are one inside the other
        assert_eq!(spans("(a*)++", "aa"), Some(vec![Some(0..2), Some(2..2)]));
        let found = spans("(()*(|a){2,})*", "a").unwrap();
        assert_eq!(found, vec![Some(0..1), Some(0..1), Some(0..0), Some(0..1)]);
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
    fn a_utf8_text_is_matched_character_by_character() {
        let utf8 = Options {
            utf8: true,
            ..Options::default()
        };
        let re = |pattern: &str| Regex::with_options(pattern.as_bytes(), utf8).unwrap();
        assert_eq!(re("^.$").find("é".as_bytes()), Some(0..2));
        assert!(!Regex::new(b"^.$").unwrap().is_match("é".as_bytes()));
        assert!(re("^[[:alpha:]_][[:alnum:]_]*$").is_match("Méthode".as_bytes()));
        assert!(re("^[^x]$").is_match("€".as_bytes()));
        assert_eq!(re("\\bé\\w+").find("x étude.".as_bytes()), Some(2..8));
        assert!(!re("d\\b").is_match("idée".as_bytes()));
        let found = re("(é+)(.)").captures("xééa".as_bytes()).unwrap();
        assert_eq!((found.get(1), found.get(2)), (Some(1..5), Some(5..6)));
        // a byte that begins no character is taken only by itself
        assert!(!re("^[^x]$").is_match(b"\xe9") && !re("^.$").is_match(b"\xe9"));
        assert!(
            Regex::with_options(b"caf\xe9", utf8)
                .unwrap()
                .is_match(b"caf\xe9")
        );
        let any_case = Options {
            ignore_case: true,
            ..utf8
        };
        let re = Regex::with_options("^été$".as_bytes(), any_case).unwrap();
        assert!(re.is_match("ÉTÉ".as_bytes()) && !re.is_match("ete".as_bytes()));
        // the locale orders no character outside ASCII
        let ranged = Regex::with_options("[é-ü]".as_bytes(), utf8);
        assert_eq!(ranged.err(), Some(Error::Collate));
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
            ("(a)$|a", "a"),
            ("(^)|x*", ""),
            ("x(^)|x()", "x"),
            ("b|(a)$|a", "ba"),
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
            let found = backtrack::captures(&re.prog, cache, text, from, to, &mut by_paths);
            assert_eq!(found, Some(true), "{pattern}");
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

    #[test]
    fn a_back_reference_matches_what_its_group_matched() {
        assert_eq!(spans("(a)\\1", "xaab"), Some(vec![Some(1..3), Some(1..2)]));
        assert_eq!(spans("(a)\\1", "aba"), None);
        assert_eq!(
            spans("^(.+)\\1$", "abcabc"),
            Some(vec![Some(0..6), Some(0..3)])
        );
        assert_eq!(spans("^(.+)\\1$", "abcabd"), None);
        // the longest match of the earliest start, not the first path's
        assert_eq!(
            spans("(a|ab)\\1", "abab"),
            Some(vec![Some(0..4), Some(0..2)])
        );
        assert_eq!(
            spans("(a*)\\1", "aaaaa"),
            Some(vec![Some(0..4), Some(0..2)])
        );
        assert_eq!(
            spans("(a)(b)\\2\\1", "abba"),
            Some(vec![Some(0..4), Some(0..1), Some(1..2)])
        );
        // turns inside turns before a reference: the ways to split the
        // words between them are many, the places to stand few
        let re = Regex::new(b"(\\w+) (\\w+ *)*\\1").unwrap();
        let line = b"the quick brown fox jumps over the lazy dog and the cat";
        assert_eq!(re.find(line), Some(0..51));
        let found = re.captures(line).unwrap();
        assert_eq!((found.get(1), found.get(2)), (Some(0..3), Some(44..48)));
        // asked for fewer groups than a reference names, the C library
        // finds no match; asked for none, it finds one
        let re = Regex::new(b"((a)\\2)").unwrap();
        assert_eq!(re.captures(b"aa").and_then(|c| c.get(2)), Some(0..1));
        assert!(re.captures_upto(b"aa", 1).is_none() && re.is_match(b"aa"));
        assert_eq!(re.find(b"aa"), Some(0..2));
        // also in a text of nothing, whichever group is referred to
        let re = Regex::new(b"(a?)(b?)(c?)((d)?)*\\4").unwrap();
        assert!(re.is_match(b"") && re.find(b"") == Some(0..0));
        assert!(
            Regex::new(b"(a)\\1")
                .unwrap()
                .captures_upto(b"aa", 1)
                .is_some()
        );
        // a turn of an outer repetition begins the inner one anew
        assert_eq!(spans("(a*)*-\\1$", "aa-a").unwrap()[0], Some(0..4));
        assert_eq!(spans("(a*)*-\\1", "aa-a").unwrap()[0], Some(0..4));
        // a group that took no part matches nothing again
        assert_eq!(spans("((a)|b)\\2", "b"), None);
        assert_eq!(spans("(a*)*\\1", "b"), Some(vec![Some(0..0), Some(0..0)]));
        let any_case = Options {
            ignore_case: true,
            ..Options::default()
        };
        let re = Regex::with_options(b"(a)\\1", any_case).unwrap();
        assert_eq!(re.find(b"xAa"), Some(1..3));
        let basic = Options {
            syntax: Syntax::Basic,
            ..Options::default()
        };
        let re = Regex::with_options(b"^\\(a*\\)b\\1$", basic).unwrap();
        assert!(re.is_match(b"aabaa") && !re.is_match(b"aaba"));
        // a group of a byte that is no character, and no text left for
        // its reference
        let stray = Options {
            utf8: true,
            ..any_case
        };
        let re = Regex::with_options(b"(\xe9)\\1", stray).unwrap();
        assert!(!re.is_match(b"\xe9") && re.captures(b"\xe9").is_none());
        assert_eq!(re.find(b"x\xe9\xe9"), Some(1..3));
    }

    #[test]
    fn a_back_reference_search_gives_up_at_its_bound_and_says_no() {
        // every way of splitting the text is tried before the end fails
        let re = Regex::new(b"^(a|aa)+\\1b$").unwrap();
        assert!(re.is_match(b"aaaab"));
        let begun = std::time::Instant::now();
        assert!(!re.is_match(&[b'a'; 200]));
        assert!(re.captures(&[b'a'; 200]).is_none());
        assert!(begun.elapsed() < std::time::Duration::from_secs(20));
    }
}
