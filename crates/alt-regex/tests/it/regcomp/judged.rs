//! The groups of repetitions without bound of what may match nothing:
//! where the C library puts them, whatever the rules of POSIX say.

use alt_regex::{Options, Regex};

use super::oracle::Oracle;
use crate::ideal::judged::{JUDGED, Verdict, ours, report, verdict};
use crate::ideal::{Rng, Writer, read, text};

#[test]
fn the_c_library_reports_the_groups_judged_as_its_own() {
    let mut oracle = Oracle::start(false);
    for (pattern, text, groups, _) in JUDGED {
        let question = (pattern.into(), Options::default(), vec![text.into()]);
        let theirs = oracle.ask(question).flatten().expect("an answer");
        let theirs = theirs[0].as_ref().expect("a match");
        let groups = report(groups);
        assert_eq!(theirs[..groups.len()], groups[..], "{pattern} on {text}");
    }
}

/// Runs `count` random patterns, in which what may match nothing is
/// repeated without bound; returns the texts compared and the matches
/// whose groups differ, each with how the two sides stand by the rules.
fn judge(seed: u64, count: usize) -> (usize, Vec<String>) {
    let mut rng = Rng(seed);
    let mut oracle = Oracle::start(false);
    let mut differing = Vec::new();
    let mut compared = 0;
    for _ in 0..count {
        let mut writer = Writer {
            rng: &mut rng,
            edges: false,
            out: Vec::new(),
        };
        writer.expression(2);
        let pattern = writer.out;
        let texts: Vec<Vec<u8>> = (0..6).map(|_| text(&mut rng)).collect();
        let re = Regex::new(&pattern).unwrap();
        if pattern.is_empty() || re.groups() >= super::GROUPS {
            continue;
        }
        let question = (pattern.clone(), Options::default(), texts.clone());
        // for some of these patterns the C library never answers
        let Some(theirs) = oracle.ask(question).flatten() else {
            continue;
        };
        let node = read(&pattern, false);
        for (text, theirs) in texts.iter().zip(theirs) {
            compared += 1;
            let here = ours(&re, text);
            let there = theirs.map(|spans| spans[..=re.groups()].to_vec());
            let shown = format!("{} on {}", pattern.escape_ascii(), text.escape_ascii());
            let whole = |report: &Option<Vec<_>>| report.as_ref().map(|r| r[0]);
            assert_eq!(whole(&here), whole(&there), "{shown}");
            let (Some(here), Some(there)) = (here, there) else {
                continue;
            };
            if here != there {
                let sides: (Verdict, Verdict) =
                    (verdict(&node, text, &here), verdict(&node, text, &there));
                differing.push(format!("{shown}: {here:?} {sides:?}, theirs {there:?}"));
            }
        }
    }
    (compared, differing)
}

#[test]
fn groups_of_repetitions_of_nothing_lie_where_the_c_library_puts_them() {
    let (compared, differing) = judge(23, 4_000);
    assert!(compared > 15_000, "{compared}");
    assert!(differing.is_empty(), "{}", differing.join("\n"));
}

#[test]
#[ignore = "a quarter of a million random patterns with every way to match each, needs --release"]
fn groups_of_repetitions_of_nothing_lie_where_the_c_library_puts_them_long() {
    for seed in [7, 11, 13, 17] {
        let (compared, differing) = judge(seed, 60_000);
        println!("seed {seed}: {} of {compared} texts", differing.len());
        assert!(compared > 250_000, "{compared}");
        assert!(differing.is_empty(), "{}", differing.join("\n"));
    }
}
