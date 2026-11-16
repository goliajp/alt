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
    // groups and the repetitions around them count together
    let wrapped = |depth: usize| {
        let mut pattern = b"(".repeat(depth);
        pattern.push(b'a');
        pattern.extend(b")?".repeat(depth));
        Regex::new(&pattern)
    };
    assert!(wrapped(100).unwrap().is_match(b"a"));
    assert_eq!(wrapped(101).err(), Some(Error::TooBig));
    let grouped = |depth: usize| ["(".repeat(depth), "a".into(), ")".repeat(depth)].concat();
    assert!(Regex::new(grouped(200).as_bytes()).is_ok());
    assert_eq!(
        Regex::new(grouped(201).as_bytes()).err(),
        Some(Error::TooBig)
    );
}

/// Runs on a thread with the stack a thread has when none is asked for.
fn on_a_small_stack<T: Send + 'static>(run: impl FnOnce() -> T + Send + 'static) -> T {
    let thread = std::thread::Builder::new().stack_size(2 << 20);
    thread.spawn(run).unwrap().join().unwrap()
}

#[test]
fn the_deepest_patterns_are_read_compiled_and_searched_on_a_small_stack() {
    // each group under as many repetitions as were left to nest
    let mut deep = b"(".repeat(199);
    deep.push(b'a');
    for group in (0..199).rev() {
        deep.push(b')');
        deep.extend(b"{2,}".repeat(199 - group));
    }
    assert_eq!(
        on_a_small_stack(move || Regex::new(&deep).err()),
        Some(Error::TooBig)
    );
    for (open, close) in [("(", ")?"), ("(", ")"), ("(x|", ")*"), ("(a|(", "))")] {
        let levels = 200 / (open.matches('(').count() + close.len() - close.matches(')').count());
        let pattern = [open.repeat(levels), "a".into(), close.repeat(levels)].concat();
        let found = on_a_small_stack(move || {
            let re = Regex::new(pattern.as_bytes()).unwrap();
            let whole = re.captures(b"a").and_then(|found| found.get(0));
            (re.is_match(b"a"), whole)
        });
        assert_eq!(found, (true, Some(0..1)), "{open}");
    }
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

#[test]
fn the_tables_of_a_search_are_bounded_in_bytes_whatever_the_states_hold() {
    // each state of this one holds some two thousand instructions
    let re = Regex::new(b"(a|b)*a(a|b){2000}c").unwrap();
    let mut text = vec![b'a'; 9_000];
    let most = crate::most_held(|| {
        assert!(!re.is_match(&text));
        assert_eq!(re.find(&text), None);
    });
    assert!(most < 24 << 20, "{most} bytes");
    text.push(b'c');
    assert_eq!(re.find(&text), Some(0..9_001));
}

#[test]
fn sets_of_characters_past_what_a_program_holds_are_refused_as_they_are_read() {
    let utf8 = Options {
        utf8: true,
        ..Options::default()
    };
    // some twenty thousand sets, no two alike
    let mut pattern = String::new();
    let mut other = ('\u{4e00}'..).filter(|c| !c.is_alphabetic());
    while pattern.len() + 24 < 1 << 18 {
        pattern.push_str(&format!("[[:alpha:]{}]", other.next().unwrap()));
    }
    let begun = Instant::now();
    let most = crate::most_held(|| {
        let refused = Regex::with_options(pattern.as_bytes(), utf8);
        assert_eq!(refused.err(), Some(Error::TooBig));
    });
    let cost = begun.elapsed();
    assert!(cost < Duration::from_secs(5), "{cost:?}");
    assert!(most < 32 << 20, "{most} bytes");
}

/// As much of `unit` written again and again, numbered, as the longest
/// pattern has room for behind `head`.
fn filled(head: &str, unit: impl Fn(u32) -> String) -> Vec<u8> {
    let mut pattern = String::from(head);
    for number in 0.. {
        let next = unit(number);
        if pattern.len() + next.len() > 1 << 18 {
            break;
        }
        pattern.push_str(&next);
    }
    pattern.into_bytes()
}

/// Compiles `pattern`, or sees it refused, holding no more than a
/// pattern may; returns what that took.
fn bounded(compiled: bool, options: Options, pattern: &[u8]) -> Duration {
    let shown = &pattern[..pattern.len().min(24)].escape_ascii();
    let begun = Instant::now();
    let most = crate::most_held(|| {
        let made = Regex::with_options(pattern, options);
        match compiled {
            true => assert!(made.is_ok(), "{shown}"),
            false => assert_eq!(made.err(), Some(Error::TooBig), "{shown}"),
        }
    });
    assert!(most < 32 << 20, "{most} bytes for {shown}");
    begun.elapsed()
}

/// Patterns as long as a pattern may be, of the kinds that hold the
/// most: whether each is compiled, how it is read, and the pattern.
fn hostile() -> Vec<(bool, Options, Vec<u8>)> {
    let bytes = Options::default();
    let utf8 = Options {
        utf8: true,
        ..bytes
    };
    let any_case = Options {
        ignore_case: true,
        ..utf8
    };
    let han = |number: u32| char::from_u32(0x4e00 + number % 20_000).unwrap();
    let same = |unit: &'static str| move |_| unit.to_string();
    // whether it is compiled, and what a pattern is made of
    vec![
        (true, bytes, filled("", same("a"))),
        (true, utf8, filled("", same("\u{e9}"))),
        (true, bytes, filled("", same("a|"))),
        (true, bytes, filled("(a)(a)(a)", same("a"))),
        (true, bytes, filled("(a?)*(b?)*(c?)*", same("a"))),
        (true, bytes, b"(()*){30000}".to_vec()),
        (true, utf8, b".{1,8000}".to_vec()),
        (true, utf8, b"\\w{1,500}".to_vec()),
        (false, any_case, filled("", same("\u{e9}"))),
        (false, any_case, filled("", same("i"))),
        (false, any_case, filled("", |n| han(n).to_string())),
        (false, utf8, filled("", same("."))),
        (false, utf8, ".".repeat(8_000).into_bytes()),
        (false, utf8, filled("", same("\\W|"))),
        (
            false,
            utf8,
            filled("", |n| format!("[[:alpha:]{}]", han(n))),
        ),
        (false, any_case, filled("", |n| format!("[^{}]|", han(n)))),
        (false, utf8, filled("", |n| format!("[{}]", han(n)))),
        (false, bytes, filled("", same("(a|b)"))),
        (false, bytes, b"((a|b){1,300}){1,300}".to_vec()),
    ]
}

#[test]
fn no_pattern_holds_more_than_its_bound_while_it_is_compiled() {
    for (compiled, options, pattern) in hostile() {
        bounded(compiled, options, &pattern);
    }
}

#[test]
#[ignore = "a limit on time, which a machine with much else to do does not keep, needs --release"]
fn no_pattern_takes_long_to_compile() {
    for (compiled, options, pattern) in hostile() {
        let cost = bounded(compiled, options, &pattern);
        let shown = &pattern[..pattern.len().min(24)].escape_ascii();
        assert!(cost < Duration::from_secs(5), "{cost:?} for {shown}");
    }
}
