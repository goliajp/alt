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
