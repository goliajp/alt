//! The groups this crate finds, which are the C library's, judged by
//! the rules of [`super::groups`]: how far the two lie apart.

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

/// Matches whose groups the rules of POSIX would put elsewhere, or
/// leave open, all in a repetition without bound of what may match
/// nothing: the pattern, the text, the groups the C library reports,
/// which are the groups this crate reports, and how they stand by the
/// rules.
pub const JUDGED: [(&str, &str, &str, Verdict); 14] = {
    use Verdict::{Neither, Rule};
    [
        // the repetition is left short of what it could take
        ("(b?|a)*a?", "ba", "0-2 0-1", Neither),
        ("(b{0,1}|[ab]{0,1})*a?", "abbaba", "0-6 4-5", Neither),
        // a group of a branch that was not taken is reported
        ("((b*)|a)*", "a", "0-1 0-1 0-0", Neither),
        ("b{,2}((b*)*|a)*b{,2}", "ba baa", "0-2 1-2 1-1", Neither),
        (
            "()|((){1,2}([ab]{0,1}b?b|)|)*b{,2}",
            "b",
            "0-1 - 0-1 0-0 1-1",
            Neither,
        ),
        (
            "(){1,2}|b?((|b{2}a+){2,}(b*))*[^a]{,2}",
            "bb",
            "0-2 - 1-2 1-1 2-2",
            Neither,
        ),
        // one more turn of nothing behind the last, which the rules
        // leave open, is taken
        ("((x|)*b){,2}", "xb", "0-2 0-2 1-1", Rule),
        ("(b+|(|a)*){2}", "ba", "0-2 1-2 2-2", Rule),
        (
            "a{,2}(ab*(b+)|(b{0,1}b?)*\\W?x{0,1}){2,}b?",
            " b",
            "0-2 1-2 - 2-2",
            Rule,
        ),
        // a group is kept as a turn of nothing found it
        (
            "(|(x{,2}|b{1,2}){2,}){2}|b{2,}b",
            "bb",
            "0-2 0-2 2-2",
            Neither,
        ),
        (
            "((b?|bb{,2}[a-c]{0,1})+( {1,2}b+){0,1}){2,}b?",
            "ba_ ba",
            "0-2 0-2 2-2 -",
            Neither,
        ),
        (
            "(b*(b?x{0,1}|b{2}a* +)*|[[:alpha:]]{2}(){1,2}()?)*",
            "abbbb",
            "0-5 0-5 0-0 2-2 2-2",
            Neither,
        ),
        // and here the rules are kept
        (
            "()|[^a]{,2}((b{,2}|ab*){2}){2,}b{0,1}",
            "baaba",
            "0-5 - 4-5 5-5",
            Rule,
        ),
        (
            "((b{0,1}|b\\w?b{1,2}){,2})*(){0,1}|b{,2}ab{2}(ab*b*){1,2}",
            "bb_",
            "0-2 0-2 2-2 2-2 -",
            Rule,
        ),
    ]
};

#[test]
fn groups_lie_where_the_c_library_puts_them_whatever_the_rules_say() {
    for (pattern, text, groups, stands) in JUDGED {
        let node = read(pattern.as_bytes(), false);
        let re = alt_regex::Regex::new(pattern.as_bytes()).unwrap();
        let found = ours(&re, text.as_bytes()).unwrap();
        assert_eq!(found, report(groups), "{pattern} on {text}");
        assert_eq!(
            verdict(&node, text.as_bytes(), &found),
            stands,
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
// two hundred that is not the way the rules name. The count says how
// far the two lie apart; it is no aim
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
