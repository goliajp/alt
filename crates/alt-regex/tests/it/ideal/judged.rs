//! The groups this crate finds, judged by the rules of [`super::groups`].

use super::groups::{Kept, Report, admitted};
use super::{Node, Rng, Writer, read, text};

/// How a report stands by the rules.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Verdict {
    /// what the rules give
    Rule,
    /// what they give but for rule 6
    Stale,
    Neither,
}

pub fn verdict(node: &Node, text: &[u8], report: &Report) -> Verdict {
    let groups = report.len() - 1;
    let admits = |kept| admitted(node, groups, text, kept).is_some_and(|all| all.contains(report));
    match (admits(Kept::Last), admits(Kept::Stale)) {
        (true, _) => Verdict::Rule,
        (false, true) => Verdict::Stale,
        (false, false) => Verdict::Neither,
    }
}

/// The groups this crate finds, from 0.
pub fn ours(re: &alt_regex::Regex, text: &[u8]) -> Option<Report> {
    let found = re.captures(text)?;
    let span = |group| found.get(group).map(|r| (r.start, r.end));
    Some((0..=re.groups()).map(span).collect())
}

/// A report as written in [`JUDGED`]: `0-3 - 2-3`.
pub fn report(written: &str) -> Report {
    let span = |span: &str| {
        let (from, to) = span.split_once('-')?;
        Some((from.parse().ok()?, to.parse().ok()?))
    };
    written.split(' ').map(span).collect()
}

/// Matches whose groups the C library reports otherwise, all in a
/// repetition without bound of what may match nothing: the pattern, the
/// text, the groups here with how they stand by the rules, and the
/// same for the C library's. The whole match is the same in each.
pub const JUDGED: [(&str, &str, &str, Verdict, &str, Verdict); 14] = {
    use Verdict::{Neither, Rule, Stale};
    [
        // the C library leaves the repetition short of what it could take
        ("(b?|a)*a?", "ba", "0-2 1-2", Rule, "0-2 0-1", Neither),
        (
            "(b{0,1}|[ab]{0,1})*a?",
            "abbaba",
            "0-6 5-6",
            Rule,
            "0-6 4-5",
            Neither,
        ),
        // or reports a group of a branch that was not taken
        ("((b*)|a)*", "a", "0-1 0-1 -", Rule, "0-1 0-1 0-0", Neither),
        (
            "b{,2}((b*)*|a)*b{,2}",
            "ba baa",
            "0-2 1-2 -",
            Rule,
            "0-2 1-2 1-1",
            Neither,
        ),
        (
            "()|((){1,2}([ab]{0,1}b?b|)|)*b{,2}",
            "b",
            "0-1 - 0-1 0-0 0-1",
            Rule,
            "0-1 - 0-1 0-0 1-1",
            Neither,
        ),
        (
            "(){1,2}|b?((|b{2}a+){2,}(b*))*[^a]{,2}",
            "bb",
            "0-2 - 1-2 1-1 1-2",
            Rule,
            "0-2 - 1-2 1-1 2-2",
            Neither,
        ),
        // one more turn of nothing behind the last, which the rules
        // leave open: the C library takes it, in copies of a group that
        // an interval makes
        (
            "((x|)*b){,2}",
            "xb",
            "0-2 0-2 0-1",
            Rule,
            "0-2 0-2 1-1",
            Rule,
        ),
        (
            "(b+|(|a)*){2}",
            "ba",
            "0-2 1-2 1-2",
            Rule,
            "0-2 1-2 2-2",
            Rule,
        ),
        (
            "a{,2}(ab*(b+)|(b{0,1}b?)*\\W?x{0,1}){2,}b?",
            " b",
            "0-2 1-2 - 1-2",
            Rule,
            "0-2 1-2 - 2-2",
            Rule,
        ),
        // neither side by the rules: both keep a group as a turn of
        // nothing found it, each in its own way
        (
            "(|(x{,2}|b{1,2}){2,}){2}|b{2,}b",
            "bb",
            "0-2 0-2 0-2",
            Neither,
            "0-2 0-2 2-2",
            Neither,
        ),
        (
            "((b?|bb{,2}[a-c]{0,1})+( {1,2}b+){0,1}){2,}b?",
            "ba_ ba",
            "0-2 0-2 0-2 -",
            Neither,
            "0-2 0-2 2-2 -",
            Neither,
        ),
        (
            "(b*(b?x{0,1}|b{2}a* +)*|[[:alpha:]]{2}(){1,2}()?)*",
            "abbbb",
            "0-5 2-5 5-5 2-2 2-2",
            Stale,
            "0-5 0-5 0-0 2-2 2-2",
            Neither,
        ),
        // the C library by the rules and this crate not
        (
            "()|[^a]{,2}((b{,2}|ab*){2}){2,}b{0,1}",
            "baaba",
            "0-5 - 4-5 4-5",
            Neither,
            "0-5 - 4-5 5-5",
            Rule,
        ),
        (
            "((b{0,1}|b\\w?b{1,2}){,2})*(){0,1}|b{,2}ab{2}(ab*b*){1,2}",
            "bb_",
            "0-2 0-2 1-2 2-2 -",
            Neither,
            "0-2 0-2 2-2 2-2 -",
            Rule,
        ),
    ]
};

#[test]
fn the_groups_the_c_library_reports_otherwise_stand_by_the_rules_as_judged() {
    for (pattern, text, here, stands, there, theirs) in JUDGED {
        let node = read(pattern.as_bytes(), false);
        let re = alt_regex::Regex::new(pattern.as_bytes()).unwrap();
        let found = ours(&re, text.as_bytes()).unwrap();
        assert_eq!(found, report(here), "{pattern} on {text}");
        assert_eq!(
            verdict(&node, text.as_bytes(), &found),
            stands,
            "{pattern} on {text}"
        );
        let there = report(there);
        assert_eq!(
            verdict(&node, text.as_bytes(), &there),
            theirs,
            "{pattern} on {text}"
        );
    }
}

/// Of the matches of `count` random patterns, how many there were and
/// how many have their groups where the rules put them.
fn compare(seed: u64, count: usize) -> (usize, usize) {
    let mut rng = Rng(seed);
    let (mut matched, mut by_rule) = (0, 0);
    for _ in 0..count {
        let mut writer = Writer {
            rng: &mut rng,
            edges: false,
            out: Vec::new(),
        };
        writer.expression(2);
        let pattern = writer.out;
        let node = read(&pattern, false);
        let re = alt_regex::Regex::new(&pattern).unwrap();
        for _ in 0..6 {
            let text = text(&mut rng);
            let Some(found) = ours(&re, &text) else {
                continue;
            };
            matched += 1;
            by_rule += usize::from(verdict(&node, &text, &found) != Verdict::Neither);
        }
    }
    (matched, by_rule)
}

// the groups are the C library's, which tries the branches of an
// alternation and the turns of a repetition in the order written and
// keeps the first way that gives the longest match: in some matches of
// two hundred that is not the way the rules name
#[test]
fn nearly_all_groups_lie_where_posix_puts_them() {
    let (matched, by_rule) = compare(0x9e3779b97f4a7c15, 6_000);
    assert!(matched > 15_000, "{matched}");
    assert!(by_rule * 200 > matched * 198, "{by_rule} of {matched}");
}

#[test]
#[ignore = "a million random patterns against a search of every way to match, needs --release"]
fn nearly_all_groups_lie_where_posix_puts_them_long() {
    let (matched, by_rule) = compare(0x2545f4914f6cdd1d, 1_000_000);
    assert!(matched > 2_500_000, "{matched}");
    assert!(by_rule * 200 > matched * 198, "{by_rule} of {matched}");
    println!("{by_rule} of {matched}");
}
