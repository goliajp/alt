//! On a file system that takes names differing in case for one, what
//! takes a checkout back, or finishes one that was cut short, leaves
//! each name spelled as the tree it ends at spells it.

use crate::casefold::{clean, code, pair, same_state, spelled};
use crate::casefold_wider::cut;
use crate::like_git::{Pair, alt, index, ok};

/// Switches from each branch to the other, both ways, takes the switch
/// back and makes it again: the names are spelled as git spells them at
/// the branch the tree is at.
fn there_and_back(pair: &Pair, branches: [&str; 2]) {
    for (from, to) in [(branches[0], branches[1]), (branches[1], branches[0])] {
        pair.both(&["switch", from]);
        same_state(pair);
        let before = (spelled(&pair.alt), index(&pair.alt));
        pair.both(&["switch", to]);
        same_state(pair);
        ok(alt(&pair.alt, &["undo"]));
        let undone = (spelled(&pair.alt), index(&pair.alt));
        assert_eq!(undone, before, "{to} back to {from}");
        clean(&pair.alt, false);
        ok(alt(&pair.alt, &["undo"]));
        same_state(pair);
        clean(&pair.alt, false);
    }
}

#[test]
#[ignore = "needs ALT_CASEFOLD_DIR: a directory on a file system that takes names differing in case for one"]
fn an_undo_gives_a_file_the_spelling_it_had() {
    let pair = pair();
    pair.both(&["switch", "-c", "up"]);
    pair.both(&["mv", "a", "A"]);
    pair.commit_all("respelled");
    pair.both(&["switch", "-c", "other"]);
    pair.write("A", "other\n");
    pair.commit_all("other content");
    there_and_back(&pair, ["main", "up"]);
    there_and_back(&pair, ["main", "other"]);
}

#[test]
#[ignore = "needs ALT_CASEFOLD_DIR: a directory on a file system that takes names differing in case for one"]
fn a_switch_that_only_respells_cut_short_ends_with_the_names_of_its_branch() {
    let pair = pair();
    pair.both(&["switch", "-c", "up"]);
    pair.both(&["mv", "a", "A"]);
    pair.commit_all("respelled");
    // the names as a switch of git leaves them, not as its `mv` does
    pair.both(&["switch", "main"]);
    pair.both(&["switch", "up"]);
    let at_up = (spelled(&pair.git), index(&pair.alt));
    pair.both(&["switch", "main"]);
    let at_main = (spelled(&pair.git), index(&pair.alt));
    assert_ne!(at_up.0, at_main.0);
    let repo = &pair.alt;
    let list = repo.join(".alt/points");
    let whole = cut(repo, &["switch", "up"], &format!("list:{}", list.display()));
    assert_eq!(code(&whole), Some(0));
    let points = std::fs::read_to_string(&list).unwrap().lines().count();
    std::fs::remove_file(&list).unwrap();
    assert!(points > 3, "{points}");
    ok(alt(repo, &["switch", "main"]));
    let mut finished = 0;
    for at in 1..=points {
        let killed = cut(repo, &["switch", "up"], &at.to_string());
        assert_eq!(code(&killed), None, "point {at}");
        let pending = ok(alt(repo, &["operation", "pending"]));
        if let Some(intent) = pending.lines().last() {
            let id = intent.split(' ').next().unwrap();
            ok(alt(repo, &["operation", "recover", id]));
        }
        assert_eq!(ok(alt(repo, &["operation", "pending"])), "", "point {at}");
        let now = (spelled(repo), index(repo));
        assert!(now == at_up || now == at_main, "point {at}: {now:?}");
        finished += usize::from(now == at_up);
        clean(repo, false);
        ok(alt(repo, &["switch", "main"]));
        assert_eq!((spelled(repo), index(repo)), at_main, "point {at}");
    }
    assert!(finished > 0 && finished < points, "{finished} of {points}");
}
