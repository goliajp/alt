//! The groups of repetitions without bound of what may match nothing,
//! where this crate and the C library now and then differ: each side
//! judged by the rules of POSIX.

use std::collections::BTreeMap;

use alt_regex::{Options, Regex};

use super::oracle::Oracle;
use crate::ideal::judged::{JUDGED, Verdict, ours, report, verdict};
use crate::ideal::{Rng, Writer, read, text};

#[test]
fn the_c_library_reports_the_groups_judged_as_its_own() {
    let mut oracle = Oracle::start(false);
    for (pattern, text, _, _, there, _) in JUDGED {
        let question = (pattern.into(), Options::default(), vec![text.into()]);
        let theirs = oracle.ask(question).flatten().expect("an answer");
        let theirs = theirs[0].as_ref().expect("a match");
        let there = report(there);
        assert_eq!(theirs[..there.len()], there[..], "{pattern} on {text}");
    }
}

/// Runs `count` random patterns; returns the texts compared and, for
/// each pair of verdicts on the two sides, the matches whose groups
/// differ.
fn judge(seed: u64, count: usize) -> (usize, BTreeMap<(Verdict, Verdict), Vec<String>>) {
    let mut rng = Rng(seed);
    let mut oracle = Oracle::start(false);
    let mut differing = BTreeMap::<_, Vec<String>>::new();
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
                let sides = (verdict(&node, text, &here), verdict(&node, text, &there));
                let shown = format!("{shown}: {here:?}, theirs {there:?}");
                differing.entry(sides).or_default().push(shown);
            }
        }
    }
    (compared, differing)
}

#[test]
#[ignore = "a quarter of a million random patterns with every way to match each, needs --release"]
fn groups_differ_from_the_c_library_in_few_matches_and_are_judged() {
    for seed in [7, 11, 13, 17] {
        let (compared, differing) = judge(seed, 60_000);
        let count: usize = differing.values().map(Vec::len).sum();
        println!("seed {seed}: {count} of {compared} texts");
        for ((here, there), mut matches) in differing {
            println!("  ours {here:?}, theirs {there:?}: {}", matches.len());
            matches.sort_by_key(String::len);
            matches.iter().take(4).for_each(|m| println!("    {m}"));
        }
        assert!(count * 2_000 < compared, "{count} of {compared}");
    }
}
