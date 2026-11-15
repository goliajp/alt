//! Patterns at and past the sizes this crate reads: each is compiled
//! within a bound on time and memory, or refused.

use std::time::{Duration, Instant};

use alt_regex::{Error, Options, Regex, Syntax};

fn within(limit: Duration, pattern: &[u8]) -> Result<Regex, Error> {
    let begun = Instant::now();
    let compiled = Regex::new(pattern);
    let cost = begun.elapsed();
    assert!(cost < limit, "{} bytes took {cost:?}", pattern.len());
    compiled
}

#[test]
fn repetition_marks_behind_one_another_are_one_repetition() {
    let mut pattern = b"^a".to_vec();
    pattern.extend(std::iter::repeat_n(b'*', 100_000));
    pattern.push(b'$');
    let re = within(Duration::from_secs(5), &pattern).unwrap();
    assert!(re.is_match(b"aaa") && re.is_match(b"") && !re.is_match(b"ab"));
    for (pattern, yes, no) in [
        ("^a+*?$", "", "b"),
        ("^a++$", "aa", ""),
        ("^a??$", "a", "aa"),
        ("^a?+$", "aaa", "b"),
        ("^(a)+?$", "", "b"),
        ("^a{1}{1}$", "a", "aa"),
    ] {
        let re = Regex::new(pattern.as_bytes()).unwrap();
        assert!(re.is_match(yes.as_bytes()), "{pattern} on {yes}");
        assert!(!re.is_match(no.as_bytes()), "{pattern} on {no}");
    }
    let basic = Options {
        syntax: Syntax::Basic,
        ..Options::default()
    };
    let mut pattern = b"a".to_vec();
    pattern.extend(std::iter::repeat_n(b'*', 100_000));
    assert!(Regex::with_options(&pattern, basic).is_ok());
}

#[test]
fn counted_repetitions_nest_to_a_depth_and_no_deeper() {
    let nested = |depth: usize| {
        let mut pattern = b"a".to_vec();
        for _ in 0..depth {
            pattern.extend(b"{1,2}");
        }
        pattern
    };
    // the size, not the depth, refuses the shallower ones
    assert!(Regex::new(&nested(4)).is_ok());
    assert_eq!(
        within(Duration::from_secs(5), &nested(100_000)).err(),
        Some(Error::TooBig)
    );
    // groups and the repetitions around them count apart
    let mut pattern = b"(".repeat(150);
    pattern.push(b'a');
    pattern.extend(b")?".repeat(150));
    assert!(Regex::new(&pattern).unwrap().is_match(b"a"));
}

#[test]
fn a_repetition_of_nothing_costs_nothing() {
    let limit = Duration::from_secs(1);
    let re = within(limit, b"^a{0}{32767}{32767}{32767}{32767}b$").unwrap();
    assert!(re.is_match(b"b") && !re.is_match(b"ab"));
    let re = within(limit, b"^(a){0}{32767}{32767}b$").unwrap();
    assert_eq!(re.captures(b"b").unwrap().get(1), None);
    assert_eq!(re.groups(), 1);
    // groups around nothing still make instructions, and too many
    let grown = within(limit, b"((a{0}){32767}){32767}");
    assert_eq!(grown.err(), Some(Error::TooBig));
    let grown = within(limit, b"((a{0}|b{0}){32767}){32767}");
    assert_eq!(grown.err(), Some(Error::TooBig));
}

#[test]
fn many_groups_are_compiled_and_searched_in_little_memory() {
    let most = crate::most_held(|| {
        let pattern = b"()".repeat(4_000);
        let re = within(Duration::from_secs(2), &pattern).unwrap();
        assert_eq!(re.groups(), 4_000);
        let found = re.captures(b"").unwrap();
        assert_eq!(found.get(4_000), Some(0..0));
        let found = re.captures(b"xyz").unwrap();
        assert_eq!((found.get(0), found.get(1)), (Some(0..0), Some(0..0)));
    });
    assert!(most < 16 << 20, "{most} bytes");
    let mut pattern = b"(a)|".repeat(300);
    pattern.extend(b"(b)\\b");
    let re = Regex::new(&pattern).unwrap();
    let most = crate::most_held(|| {
        let found = re.captures(b"ab").unwrap();
        assert_eq!((found.get(1), found.get(301)), (Some(0..1), None));
    });
    assert!(most < 16 << 20, "{most} bytes");
}

#[test]
fn the_number_of_groups_and_the_length_of_a_pattern_are_bounded() {
    assert!(Regex::new(&b"()".repeat(32_767)).is_ok());
    assert_eq!(Regex::new(&b"()".repeat(32_768)).err(), Some(Error::TooBig));
    // more groups than rows of capture slots may be kept for
    let mut wide = b"(a)|".repeat(2_000);
    wide.push(b'b');
    assert_eq!(Regex::new(&wide).err(), Some(Error::TooBig));
    assert!(Regex::new(&b"a".repeat(1 << 18)).is_ok());
    let long = within(Duration::from_secs(1), &b"a".repeat((1 << 18) + 1));
    assert_eq!(long.err(), Some(Error::TooBig));
}
