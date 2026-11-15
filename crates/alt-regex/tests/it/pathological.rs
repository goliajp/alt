//! Patterns that take a backtracking matcher exponential time.

use std::time::{Duration, Instant};

use alt_regex::Regex;

#[test]
#[ignore = "long texts against a time limit, needs --release"]
fn nested_repetitions_take_time_linear_in_the_text_long() {
    let limit = Duration::from_secs(20);
    for pattern in [
        "(a*)*[bc]",
        "(a|aa)+$",
        "^(a+)+$",
        "(a?){40}a{40}[bc]",
        "((a{1,3}){1,3}){1,3}[bc]",
        "\\b(a|aa)+\\b[bc]",
    ] {
        let re = Regex::new(pattern.as_bytes()).unwrap();
        let mut costs = Vec::new();
        for len in [100_000usize, 400_000] {
            // every prefix matches the repetition and nothing ends the match
            let mut text = vec![b'a'; len];
            text.push(b'd');
            let begun = Instant::now();
            assert!(!re.is_match(&text), "{pattern}");
            assert!(re.captures(&text).is_none(), "{pattern}");
            *text.last_mut().unwrap() = b'b';
            let found = re.captures(&text);
            let whole = matches!(pattern, "(a|aa)+$" | "^(a+)+$");
            assert_eq!(found.is_some(), !whole, "{pattern}");
            let cost = begun.elapsed();
            assert!(cost < limit, "{pattern} on {len} bytes took {cost:?}");
            costs.push(cost);
        }
        // four times the text within sixteen times the time, with slack
        // for the first run building its tables
        let bound = costs[0] * 16 + Duration::from_millis(200);
        assert!(costs[1] < bound, "{pattern}: {costs:?}");
    }
}

#[test]
fn a_pattern_that_explodes_when_backtracked_is_answered_at_once() {
    let re = Regex::new(b"(a*)*[bc]").unwrap();
    let text = vec![b'a'; 2_000];
    assert!(!re.is_match(&text));
    assert!(re.captures(&text).is_none());
    let re = Regex::new(b"\\b(a|aa)+\\b[bc]").unwrap();
    assert!(!re.is_match(&text));
}
