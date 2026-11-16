//! Where the groups of a match lie when they could lie in more than one
//! place.

use alt_regex::{Options, Regex};

use super::Theirs;

/// Patterns whose groups could lie in more than one place.
const AMBIGUOUS: [(&str, &str); 66] = [
    // a first branch of nothing is tried behind the second
    ("|(b?)", ""),
    ("a(|(b?))", "a"),
    ("(|a)+(a*b)", "aaab"),
    ("(a|)+(a*b)", "aaab"),
    ("(()|a)+(a*b)", "aaab"),
    ("(a{0}b{0}|(b?))", ""),
    ("(|(a)|(b?))", ""),
    ("((a?)||(b?))", ""),
    ("(|a|ab)(b?)", "ab"),
    ("(|(a))(|(a))", "a"),
    ("(|(a)|b)*", "ab"),
    ("(a|ab)(c|bcd)(d*)", "abcd"),
    ("^([a-z]+|[a-z]+ [a-z]+)( .*)?$", "foo bar baz"),
    ("(a|ab)(bc|c)?", "abc"),
    ("((a)|b)*", "ab"),
    ("(a*)*", "b"),
    ("(a*)+", "b"),
    ("(a*)*b", "aab"),
    // fewer copies than a count allows
    ("(.*)?ab{0,1}(.+){,2}$", "bbcca.c x"),
    ("(.ab{2})*([^ab]+){,2}", "cxaa.cba "),
    ("^(([^a]+)+){,2}.+", "bxc.ba. "),
    ("c\\W?(([b-c]{,2}.[^ab]+){1,2}){,2}", "axcxxa "),
    ("b*|([ab]{0,1}[^a]{1,2}[^ab]+){,2}[^[:space:]b]?", "bc. .bb"),
    // a turn of a repetition that takes nothing
    ("b(a{0,1}){1,2}", "abacc."),
    ("(\\(*[b-c]?){1,2}$", "bab"),
    ("((c*)?\\({,2}(.?){2}){1,2}a{0,1}", "ca"),
    (
        "((a{,2}a{0,1})?){1,2}(.{1,2}a*)?(.?[]a]+\\(){0,1}$",
        "bb. baa b",
    ),
    ("^(a{2}([^[:space:]b]?\\.{0,1}){1,2}){1,2} {,2}", "aacb"),
    ("([[:alpha:]]{2,}|c{,2}c{,2}x?){2,}x{0,1}", "x.a .b"),
    ("(\\s{0,1}b*[ab]*){1,2}$", "a"),
    // a copy of a repetition, in which no group is marked
    ("((c*)?\\({,2}(.?){2}){1,2}a{0,1}", "b abax"),
    ("(\\w{,2}[ab]{,2}){,2}", "b b."),
    ("^(b?[^a]{,2}){2}([a-c]?){,2}", ".a bba.x"),
    ("[[:alpha:]]((b*){1,2}a[^a]{0,1})+(c*)", "aa.ab."),
    ("([ab](c{0,1}){1,2}.{0,1}){2,}\\.*", ".a.b ab"),
    // a repetition that takes nothing at all
    ("\\.?( ab{2,}[[:alpha:]]|b?c{0,1})*", "a"),
    ("^.{2}(([^a]{1,2})+[a-c]{1,2}c*|c{,2})*[a-c]?", "bab"),
    ("(a?)*(b?)+", "x"),
    // what may match nothing, repeated without bound: the first way
    // that leads to the end is walked, and a turn of nothing is left
    ("(b?|a)*a?", "ba"),
    ("(b{0,1}|[ab]{0,1})*a?", "bbaxaa"),
    ("((b*)|a)*", "a"),
    ("((x|)*b){,2}", "xb"),
    ("(b+|(|a)*){2}", "ba"),
    ("(a*)*(b|ab)", "aaab"),
    // of two copies or more that must be there the last is marked as
    // one that may be left out
    ("(()*(|a){2,})*", "a"),
    ("(()*(|a){3,})*", "a"),
    ("(()+(|ab){2,4})*", "ab"),
    ("(()+(b{0,1}|ab*x?b?){2,})*", "aaab"),
    ("(()+(b?|ab){2,})*", "b"),
    // a branch that ends behind a condition, with nothing taken since,
    // gives way to one that does not
    ("(a)$|a", "a"),
    ("(a\\>)|a", "a"),
    ("^(a*)|b", "a"),
    ("(^)|x*", ""),
    ("(^)|($)", ""),
    ("($)|(^)", ""),
    ("(a)$|(a)\\>", "a"),
    ("(^a)|(a)", "a"),
    ("(a$)|(a)", "a"),
    ("(a)($)|(a)", "a"),
    ("((a)$)|(a)", "a"),
    ("(a$|a)", "a"),
    ("(a$|a)($)", "a"),
    ("x(^)|x()", "x"),
    ("^a*a*|(b?)\\.{,2}", "xa"),
    ("^(a*)|a*[^ab]{0,1}a?|a*ac$", "bcx. bbba"),
    ("b|(a)$|a", "ba"),
];

#[test]
fn groups_lie_where_the_c_library_puts_them() {
    let mut differ = Vec::new();
    // with lines told apart the groups are found the other way
    let lines = Options {
        newline: true,
        ..Options::default()
    };
    let both = [Options::default(), lines];
    for (options, (pat, text)) in both.iter().flat_map(|o| AMBIGUOUS.map(|case| (*o, case))) {
        let theirs = Theirs::new(pat.as_bytes(), options);
        let ours = Regex::with_options(pat.as_bytes(), options).unwrap();
        let expected = theirs.spans(text.as_bytes());
        let expected = expected.map(|spans| spans[..=ours.groups()].to_vec());
        let found = ours.captures(text.as_bytes()).map(|found| {
            let span = |i| found.get(i).map(|r| (r.start, r.end));
            (0..=ours.groups()).map(span).collect::<Vec<_>>()
        });
        if found != expected {
            differ.push(format!("{pat:?} on {text:?}: {found:?} not {expected:?}"));
        }
    }
    assert!(differ.is_empty(), "{}", differ.join("\n"));
}
