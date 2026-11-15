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
