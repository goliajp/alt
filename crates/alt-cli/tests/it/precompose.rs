//! `core.precomposeUnicode`, on macOS: a name a directory lists
//! decomposed is tracked composed, as git tracks it.

use crate::like_git::{Pair, alt, git, index, ok};

const COMPOSED: &str = "caf\u{e9}";
const DECOMPOSED: &str = "cafe\u{301}";

fn clean(pair: &Pair) {
    let said = ok(alt(&pair.alt, &["status"]));
    assert!(
        said.contains("nothing to commit, working tree clean"),
        "{said}"
    );
    assert_eq!(ok(git(&pair.git, &["status", "--porcelain"])), "");
}

#[test]
fn a_new_store_and_an_imported_one_are_told_that_names_are_composed() {
    let pair = Pair::new();
    let set = ["config", "core.precomposeunicode"];
    assert_eq!(ok(git(&pair.git, &set)), "true\n");
    assert_eq!(ok(alt(&pair.alt, &set)), "true\n");
    // a repository that came from where git does not set it
    std::fs::write(pair.git.join("f"), "f\n").unwrap();
    ok(git(&pair.git, &["add", "f"]));
    ok(git(&pair.git, &["commit", "-q", "-m", "one"]));
    ok(git(
        &pair.git,
        &["config", "--unset", "core.precomposeunicode"],
    ));
    let to = pair.git.parent().unwrap().join("imported");
    ok(alt(&pair.git, &["import", to.to_str().unwrap()]));
    assert_eq!(ok(alt(&to, &set)), "true\n");
    // and one that says no is left to say so
    ok(git(
        &pair.git,
        &["config", "core.precomposeunicode", "false"],
    ));
    let to = pair.git.parent().unwrap().join("told");
    ok(alt(&pair.git, &["import", to.to_str().unwrap()]));
    assert_eq!(ok(alt(&to, &set)), "false\n");
}

#[test]
fn a_name_listed_decomposed_is_tracked_composed() {
    let pair = Pair::new();
    pair.write(DECOMPOSED, "x\n");
    pair.write(&format!("d/{DECOMPOSED}"), "y\n");
    pair.both(&["add", "."]);
    pair.assert_same_state();
    let staged = index(&pair.alt);
    assert!(staged.contains("caf\\303\\251"), "{staged}");
    assert!(!staged.contains("cafe"), "{staged}");
    pair.commit_all("one");
    pair.assert_same_state();
    clean(&pair);
    // what an export of it is to git
    let out = crate::like_git::export(&pair.alt);
    let exported = out.path().join("exported");
    ok(git(&exported, &["checkout", "-q", "main", "--", "."]));
    assert_eq!(ok(git(&exported, &["status", "--porcelain"])), "");
    // changed under the name the directory has for it
    pair.write(DECOMPOSED, "changed\n");
    let said = ok(alt(&pair.alt, &["status"]));
    assert!(
        said.contains("modified:") && !said.contains("Untracked"),
        "{said}"
    );
    pair.both(&["add", "."]);
    pair.commit_all("two");
    pair.assert_same_state();
    clean(&pair);
}

#[test]
fn a_path_given_decomposed_names_the_file() {
    let pair = Pair::new();
    pair.write(DECOMPOSED, "x\n");
    pair.write("other", "o\n");
    pair.both(&["add", DECOMPOSED]);
    pair.assert_same_state();
    pair.commit_all("one");
    pair.write(DECOMPOSED, "changed\n");
    pair.both(&["add", DECOMPOSED]);
    pair.assert_same_state();
    pair.both(&["rm", "-q", "--cached", DECOMPOSED]);
    pair.assert_same_state();
}

#[test]
fn a_tree_that_spells_a_name_decomposed_is_not_checked_out_over_the_composed_file() {
    let pair = Pair::new();
    pair.write("keep", "base\n");
    pair.commit_all("base");
    pair.both(&["switch", "-c", "composed"]);
    pair.write(COMPOSED, "x\n");
    pair.commit_all("composed");
    // a tree as a system that does not compose names leaves it
    pair.both(&["switch", "-c", "decomposed", "main"]);
    pair.both(&["config", "core.precomposeunicode", "false"]);
    pair.write(DECOMPOSED, "x\n");
    pair.commit_all("decomposed");
    pair.both(&["config", "core.precomposeunicode", "true"]);
    let listed = ok(git(&pair.git, &["ls-tree", "--name-only", "decomposed"]));
    assert!(listed.contains("cafe\\314\\201"), "{listed}");
    assert_eq!(
        ok(alt(&pair.alt, &["rev-parse", "decomposed"])),
        ok(git(&pair.git, &["rev-parse", "decomposed"]))
    );
    pair.both(&["switch", "main"]);
    pair.both(&["switch", "composed"]);
    let (ours, theirs) = pair.run(&["switch", "decomposed"]);
    assert_eq!(
        (ours.status.code(), theirs.status.code()),
        (Some(1), Some(1))
    );
    pair.assert_same_state();
}

/// Names that tell the conversion git makes from Normalization Form C,
/// and names the decoder git reads through does not take.
const TELLING: [&str; 30] = [
    "\u{212b}",
    "\u{f9dc}",
    "\u{958}",
    "\u{37e}",
    "\u{11099}\u{110ba}",
    "a\u{315}\u{300}",
    "e\u{301}",
    "\u{1100}\u{1161}\u{11a8}",
    "\u{1100}\u{1161}",
    "\u{ac00}\u{11a8}",
    "a\u{302}\u{301}",
    "a\u{301}\u{302}",
    "a\u{323}\u{302}",
    "q\u{307}\u{323}",
    "\u{304b}\u{3099}",
    "\u{915}\u{93c}",
    "\u{5d9}\u{5b4}",
    "\u{1b05}\u{1b35}",
    "\u{2add}\u{338}",
    "A\u{30a}",
    "\u{2126}",
    "\u{fb01}",
    "\u{308}\u{301}",
    "e\u{301}\u{1f600}",
    "\u{1f600}",
    "\u{e2}\u{301}",
    "\u{f71}\u{f72}",
    "\u{9c7}\u{9be}",
    "x\u{301}\u{301}",
    "o\u{308}\u{304}",
];

#[test]
fn every_name_is_tracked_with_the_bytes_git_tracks_it_with() {
    let pair = Pair::new();
    let mut names: Vec<String> = TELLING.iter().map(|name| (*name).to_owned()).collect();
    // every pair the decoder puts together, and each the other way round
    for [first, second, _] in alt_precompose::pairs() {
        names.push(format!("{first}{second}"));
        names.push(format!("{second}{first}"));
    }
    let mut written = 0;
    for (at, name) in names.iter().enumerate() {
        let file = format!("n{at}-{name}-");
        let made = [&pair.alt, &pair.git].map(|root| std::fs::write(root.join(&file), "x\n"));
        // a name the file system does not take is in neither
        match made {
            [Ok(()), Ok(())] => written += 1,
            _ => {
                for root in [&pair.alt, &pair.git] {
                    let _ = std::fs::remove_file(root.join(&file));
                }
            }
        }
    }
    assert!(written > names.len() / 2, "{written} of {}", names.len());
    pair.both(&["add", "."]);
    let (ours, theirs) = (index(&pair.alt), ok(git(&pair.git, &["ls-files", "-s"])));
    assert_eq!(ours.lines().count(), written);
    for (ours, theirs) in ours.lines().zip(theirs.lines()) {
        assert_eq!(ours, theirs);
    }
    assert_eq!(ours.lines().count(), theirs.lines().count());
    pair.commit_all("named");
    pair.assert_same_state();
    clean(&pair);
}
