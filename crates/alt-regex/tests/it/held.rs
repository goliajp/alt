//! What a search holds: bounded whatever the pattern and however long
//! the text.

use std::time::Duration;

use alt_regex::{Cache, Error, Options, Regex};

/// The bound the documentation names for one search.
const MOST: usize = 48 << 20;

/// The most bytes the three kinds of search of `text` hold; `None` for
/// a pattern this crate does not compile.
fn held(pattern: &[u8], options: Options, text: &[u8]) -> Option<usize> {
    let re = Regex::with_options(pattern, options).ok()?;
    let begun = crate::thread_time();
    let most = crate::most_held(|| {
        let whole = re.find(text);
        assert_eq!(re.is_match(text), whole.is_some());
        // all the groups may be more than a search carries
        let found = re.try_captures_upto_with(&mut Cache::default(), text, re.groups());
        match found {
            Ok(found) => assert_eq!(found.and_then(|found| found.get(0)), whole),
            Err(given_up) => assert_eq!(given_up, Error::Exhausted),
        }
        // a reference to a group that is not asked for matches nothing
        let beyond = pattern
            .windows(2)
            .any(|w| w[0] == b'\\' && (b'2'..=b'9').contains(&w[1]));
        let first = re.captures_upto(text, 1);
        assert!(beyond || first.and_then(|found| found.get(0)) == whole);
    });
    let cost = crate::thread_time() - begun;
    let shown = &pattern[..pattern.len().min(32)].escape_ascii();
    assert!(cost < Duration::from_secs(120), "{cost:?} for {shown}");
    assert!(most < MOST, "{most} bytes for {shown}");
    Some(most)
}

#[test]
fn many_groups_before_a_long_run_are_searched_within_the_bound() {
    let text = [b'a'; 2_100];
    let groups = b"(a)".repeat(1_000);
    let run = [b'a'; 1_040];
    let cache = &mut Cache::default();
    for head in [&b""[..], b"(a?)*"] {
        for tail in [&b""[..], b"\\b"] {
            let pattern = [head, &groups, &run, tail].concat();
            let re = Regex::new(&pattern).unwrap();
            let first = re.captures_upto(&text, 1).unwrap();
            let whole = first.get(0).unwrap();
            let expected = match (head.is_empty(), tail.is_empty()) {
                (true, true) => 0..2_040,
                (true, false) => 60..2_100,
                (false, _) => 0..2_100,
            };
            assert_eq!(whole, expected);
            let all = re.try_captures_upto_with(cache, &text, re.groups());
            match head.is_empty() {
                // one path from each start
                true => {
                    let all = all.unwrap().unwrap();
                    let last = whole.start + 999;
                    assert_eq!(all.get(1_000), Some(last..last + 1));
                }
                // behind the loop a path for each number of its turns,
                // which with all the groups is more than is carried
                false => assert_eq!(all.err(), Some(Error::Exhausted)),
            }
            let most = held(&pattern, Options::default(), &text);
            assert!(most.is_some_and(|most| most < 24 << 20), "{most:?}");
        }
    }
}

#[test]
fn groups_past_what_the_paths_of_one_start_carry_are_an_error() {
    // behind the first byte a thousand paths wait, each with the slots
    // of a thousand groups
    let pattern = [&b"(a)b|".repeat(1_000)[..], b"(a)c\\b"].concat();
    let re = Regex::new(&pattern).unwrap();
    let cache = &mut Cache::default();
    let all = re.try_captures_upto_with(cache, b"xac", re.groups());
    assert_eq!(all.err(), Some(Error::Exhausted));
    assert_eq!(re.captures(b"xac"), None);
    // the match and its first groups are found all the same
    assert!(re.is_match(b"xac"));
    assert_eq!(re.find(b"xac"), Some(1..3));
    let few = re
        .try_captures_upto_with(cache, b"xac", 300)
        .unwrap()
        .unwrap();
    assert_eq!((few.get(0), few.get(1)), (Some(1..3), None));
    held(&pattern, Options::default(), b"xac");
}

#[test]
fn a_path_that_leaves_much_behind_it_is_not_kept() {
    let run = vec![b'a'; 1 << 19];
    // the groups of a long match, with a choice and eight slots to put
    // back for each byte of it
    let most = held(b"((((a))))*", Options::default(), &run);
    assert!(most.is_some_and(|most| most < 24 << 20), "{most:?}");
    // and the same on the way to a reference that then matches nothing
    let most = held(b"((((a))))*\\1[bc]", Options::default(), &run);
    assert!(most.is_some_and(|most| most < 40 << 20), "{most:?}");
}

/// Patterns that make the most of each byte, searched in `length` bytes.
fn long(length: usize) {
    let long: Vec<u8> = b"ab".iter().copied().cycle().take(length).collect();
    let utf8 = Options {
        utf8: true,
        ..Options::default()
    };
    let wide = "(.)|".repeat(40) + "x";
    let nested = "(".repeat(60) + "a?" + &")*".repeat(60) + "b?";
    let patterns: [(&str, Options); 14] = [
        // every byte a turn of a loop with a choice and groups in it
        ("((a)|(b))*", Options::default()),
        ("^((a)|(b))*$", Options::default()),
        ("((a)|(b))*\\b", Options::default()),
        ("(((a?)|(b?))*)*", Options::default()),
        ("((a*)*|(b*)*)*c?", Options::default()),
        ("(a|b)*(a|b)*(a|b)*c", Options::default()),
        ("(()|a|b)*\\>", Options::default()),
        (&wide, utf8),
        (&nested, Options::default()),
        // what a group took, taken again
        ("^((a|b)*)\\1$", Options::default()),
        ("((a|b)*)\\2c", Options::default()),
        ("(a|b)*()\\2*c", Options::default()),
        ("^(.*)(.*)\\2$", utf8),
        ("((ab)*)*\\1x", Options::default()),
    ];
    for (pattern, options) in patterns {
        let most = held(pattern.as_bytes(), options, &long);
        assert!(most.is_some(), "{pattern}");
    }
}

#[test]
fn a_long_text_is_searched_within_the_bound_whatever_the_pattern() {
    long(1 << 14);
}

#[test]
#[ignore = "a megabyte of text for each pattern, needs --release"]
fn a_megabyte_is_searched_within_the_bound_whatever_the_pattern() {
    long(1 << 20);
}

/// Patterns of `size` bytes, the kinds that make the most of a search,
/// each searched in `length` bytes of two texts.
fn largest(size: usize, length: usize) {
    let text: Vec<u8> = b"ab".iter().copied().cycle().take(length).collect();
    let run = vec![b'a'; length];
    let fill = |head: &str, unit: &str, tail: &str| {
        let count = (size - head.len() - tail.len()) / unit.len();
        [head, &unit.repeat(count), tail].concat()
    };
    let patterns = [
        fill("", "a", ""),
        fill("(x)|", "a", ""),
        fill("", "a", "\\b"),
        fill("(a?)*", "a", ""),
        fill("", "a?", ""),
        fill("", "a?", "$"),
        fill("\\b", "a?", "\\b"),
        fill("", "a|", "b"),
        fill("(", "a|", "b)*\\b"),
        fill("", "()", ""),
        fill("(a?)*\\b", "()", ""),
        fill("((a)|(b)|", "()", ")*"),
        fill("(", "(a?)", ")*"),
        fill("(", "((a?)*)", ")*\\b"),
        fill("(", "a?|", "b)*c"),
        fill("(a|b)*a", "(a|b)", "c"),
        fill("", "(a)b|", "(a)c"),
        format!("(a|b)*a(a|b){{{}}}c", size / 16),
        format!("((a|b)*a(a|b){{{}}}c|^(()|(a))*$)", size / 32),
        "((()|a){1,300}){1,300}".into(),
        format!("(a?){{{}}}\\b", size / 9),
        format!("((a?)*){{{}}}b?", size / 260),
    ];
    for pattern in patterns {
        for text in [&text, &run] {
            held(pattern.as_bytes(), Options::default(), text);
        }
    }
}

#[test]
fn large_patterns_are_searched_within_the_bound() {
    largest(1 << 10, 300);
}

#[test]
#[ignore = "the largest patterns, each instruction stepped for each byte, needs --release"]
fn the_largest_patterns_are_searched_within_the_bound() {
    largest(1 << 18, 1_500);
}
