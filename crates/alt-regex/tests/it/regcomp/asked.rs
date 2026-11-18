//! What both sides answer when asked for fewer groups than a pattern
//! refers to.

use alt_regex::{Options, Regex};

use super::Theirs;

/// Git asks for the first group only. The C library then keeps no
/// other, and a reference to another is what its walk through the
/// match cannot follow.
#[test]
fn a_reference_to_a_group_not_asked_for_is_answered_as_the_c_library_answers() {
    let patterns = [
        // no choice in the pattern: the reference is taken to be empty
        "((a)\\2)",
        "(a)()\\2",
        "(a)(b)\\2",
        "(a)(b{0})\\2b",
        // a choice: the reference fails, and so does the search
        "(a)(b?)\\2",
        "(a)(b)\\2c*",
        "(a)(b)\\2|(a)b",
        "(a+)(b)\\2|ab",
        "(a)(b)\\2|(c)",
        "^([a-z]+) ([a-z]+) \\2",
        // unless the walk ends before it comes to the reference
        "(x*)*(a)\\2",
        "(a)(B?)*_(.+\\2)+",
        // a turn that takes nothing while the group is open
        "(a|x*)*(b)\\2",
        "(a(x+)*)(b)\\3",
        // the group asked for is referred to as well
        "(a*)(b)\\2\\1",
        "(a)\\1(b)\\2",
        // the walk ends with the group begun and not ended
        "(a(x*)*)(b)\\2",
        "(([^0|]?)*)b?\\2",
        // a way taken up is not recorded
        "((a)\\2|y*)*",
        "(a+(b*|a*)?)*.(\\2b)",
        // a turn inside a turn, each left with nothing taken
        "(\\w+((B)*0)**)\\2+",
        "a.?|((,*)*)(a*.\\2?)*",
        // two groups with nothing between them are one
        "((b?a))1*\\2",
        // repetitions behind one another are turns inside turns
        "([Zx]b)|(()b+**\\3+)|c",
    ];
    let texts = [
        "",
        "a",
        "aa",
        "ab",
        "abb",
        "abbb",
        "abbc",
        "aabb",
        "abba",
        "abbaa",
        "xabb",
        "b",
        "bb",
        "a_b1aa1A",
        " _a_b1aa1A",
        "a00b0AB_",
        "ab ab ab",
        "ab cd cd",
        "axbb",
        "abc",
        "axb",
        "y",
        "yy",
        ",_",
        "a00b0",
        "_bx",
        "1a",
        "aab",
        " aa a",
        "b1baa b1",
        "baa",
        "xb",
    ];
    let mut differ = Vec::new();
    for pattern in patterns {
        let theirs = Theirs::new(pattern.as_bytes(), Options::default());
        let ours = Regex::new(pattern.as_bytes()).unwrap();
        for text in texts {
            let expected = theirs.registers(text.as_bytes(), 2);
            // a group the C library gives a start and no end is none
            let ended = |span: &(usize, usize)| span.0 <= span.1 && span.1 != usize::MAX;
            let whole = |span: &Option<(usize, usize)>| span.filter(ended);
            let expected = expected.map(|spans| spans.iter().map(whole).collect::<Vec<_>>());
            let found = ours.captures_upto(text.as_bytes(), 1).map(|found| {
                let span = |i| found.get(i).map(|r| (r.start, r.end));
                vec![span(0), span(1)]
            });
            if found != expected {
                differ.push(format!(
                    "{pattern:?} on {text:?}: {found:?} not {expected:?}"
                ));
            }
        }
    }
    assert!(differ.is_empty(), "{}", differ.join("\n"));
}

/// The cases the documentation lists as answered otherwise when one
/// group is asked for stay true of the C library at hand.
#[test]
fn asked_for_one_group_the_c_library_answers_otherwise_where_the_documentation_says_so() {
    for (pattern, text, _) in crate::differences::ASKED {
        let theirs = Theirs::new(pattern, Options::default()).registers(text, 2);
        // a group begun and not ended has no end there
        let theirs = theirs.map(|spans| (spans[0], spans[1]));
        let ours = crate::differences::first(pattern, text);
        let shown = format!("{} on {}", pattern.escape_ascii(), text.escape_ascii());
        assert_ne!(
            ours,
            theirs.map(|(whole, first)| (whole.unwrap(), first)),
            "{shown}"
        );
    }
    // the same pattern and text, asked for no group and for all
    let theirs = Theirs::new(b"(a(b)\\2)?+c", Options::default());
    assert_eq!(theirs.registers(b"abbc", 1), Some(vec![Some((0, 4))]));
    assert_eq!(theirs.registers(b"abbc", 2), None);
    let all = Some(vec![Some((0, 4)), Some((0, 3)), Some((1, 2))]);
    assert_eq!(theirs.registers(b"abbc", 3), all);
}
