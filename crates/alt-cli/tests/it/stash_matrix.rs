//! What `alt stash` puts away and brings back of one path, for each way
//! HEAD, the index and the file can stand to each other, beside git.

use std::os::unix::fs::PermissionsExt;

use crate::like_git::{Pair, alt, files, index, ok};
use crate::of_late;
use crate::stash::assert_same_stack;

/// One path for each case, named after it: what HEAD has, what the index
/// has, what the file is. `h`, `i` and `w` are three contents.
fn combined() -> Pair {
    let pair = Pair::new();
    pair.write(".gitignore", "ign*\n");
    let committed = [
        "h_h_w",
        "h_i_i",
        "h_i_w",
        "h_i_h",
        "h_h_gone",
        "h_gone_gone",
        "h_gone_w",
        "h_gone_h",
        "h_h_exec",
        "h_exec_exec",
        "h_h_link",
        "h_link_link",
        "h_i_gone",
    ];
    for name in committed {
        pair.write(name, "h\n");
    }
    pair.commit_all("base commit");
    let each = |act: &dyn Fn(&std::path::Path)| [&pair.alt, &pair.git].map(|root| act(root));
    let exec = |name: &str| {
        each(&|root| {
            let mode = std::fs::Permissions::from_mode(0o755);
            std::fs::set_permissions(root.join(name), mode).unwrap()
        });
    };
    let link = |name: &str| {
        each(&|root| {
            std::fs::remove_file(root.join(name)).unwrap();
            std::os::unix::fs::symlink("target", root.join(name)).unwrap()
        });
    };
    pair.write("h_h_w", "w\n");
    pair.write("h_i_i", "i\n");
    pair.both(&["add", "h_i_i"]);
    pair.write("h_i_w", "i\n");
    pair.both(&["add", "h_i_w"]);
    pair.write("h_i_w", "w\n");
    pair.write("h_i_h", "i\n");
    pair.both(&["add", "h_i_h"]);
    pair.write("h_i_h", "h\n");
    pair.remove("h_h_gone");
    pair.both(&["rm", "-q", "h_gone_gone"]);
    pair.both(&["rm", "-q", "--cached", "h_gone_w", "h_gone_h"]);
    pair.write("h_gone_w", "w\n");
    for (name, staged, file) in [("none_i_i", "i\n", "i\n"), ("none_i_w", "i\n", "w\n")] {
        pair.write(name, staged);
        pair.both(&["add", name]);
        pair.write(name, file);
    }
    pair.write("none_i_gone", "i\n");
    pair.both(&["add", "none_i_gone"]);
    pair.remove("none_i_gone");
    pair.write("none_none_w", "w\n");
    pair.write("ignored", "w\n");
    exec("h_h_exec");
    exec("h_exec_exec");
    pair.both(&["add", "h_exec_exec"]);
    link("h_h_link");
    link("h_link_link");
    pair.both(&["add", "h_link_link"]);
    pair.write("h_i_gone", "i\n");
    pair.both(&["add", "h_i_gone"]);
    pair.remove("h_i_gone");
    pair.assert_same_state();
    pair
}

#[test]
fn each_combination_is_put_away_and_comes_back_as_by_git() {
    let pop: &[&str] = &["stash", "pop"];
    let apply: &[&str] = &["stash", "apply", "--index"];
    for (push, back) in [
        (&["stash"][..], pop),
        (&["stash", "-u"], pop),
        (&["stash"], apply),
        (&["stash", "-u"], apply),
    ] {
        let told = format!("{push:?} {back:?}");
        let pair = combined();
        pair.same(push);
        pair.assert_same_state();
        assert_same_stack(&pair);
        let (ours, theirs) = pair.run(back);
        // a file the index had lost is put away twice by `-u`, as tracked
        // and as untracked, and the second is in the way of the first
        let failed = push.len() == 2;
        of_late::same_status(&ours, &theirs, if failed { 128 } else { 0 }, &told);
        if failed {
            let said = String::from_utf8(ours.stderr).unwrap();
            let git_said = String::from_utf8(theirs.stderr).unwrap();
            assert!(said.starts_with(&git_said), "{told}: {said}");
        }
        pair.assert_same_state();
        assert_same_stack(&pair);
    }
}

#[test]
fn a_file_the_index_lost_is_put_away_with_what_it_holds() {
    let pair = Pair::new();
    pair.write("f", "1\n");
    pair.write("g", "x\n");
    pair.commit_all("base commit");
    pair.write("f", "mine\n");
    pair.both(&["rm", "-q", "--cached", "f"]);
    pair.same(&["stash"]);
    assert_eq!(pair.same(&["stash", "show"]).lines().count(), 2);
    pair.same(&["stash", "show", "-p"]);
    pair.both(&["stash", "pop"]);
    pair.assert_same_state();
    assert_eq!(
        std::fs::read_to_string(pair.alt.join("f")).unwrap(),
        "mine\n"
    );
}

#[test]
fn every_combination_put_away_is_back_after_an_undo() {
    for push in [&["stash"][..], &["stash", "-u"]] {
        let pair = combined();
        let before = (index(&pair.alt), files(&pair.alt));
        ok(alt(&pair.alt, push));
        ok(alt(&pair.alt, &["undo"]));
        assert_eq!((index(&pair.alt), files(&pair.alt)), before, "{push:?}");
        assert_eq!(ok(alt(&pair.alt, &["stash", "list"])), "", "{push:?}");
    }
}
