//! `core.fileMode` off: the executable bit of a file is no change to
//! `status`, `diff` or `add`, as for git.

use std::os::unix::fs::PermissionsExt;

use crate::like_git::{Pair, alt, alt_daemon, git, index, ok};

/// `plain` and `tool`, the second executable, committed in both.
fn tree() -> Pair {
    let pair = Pair::new();
    pair.write("plain", "plain\n");
    pair.write("tool", "tool\n");
    chmod(&pair, "tool", 0o755);
    pair.commit_all("base");
    pair
}

fn chmod(pair: &Pair, path: &str, mode: u32) {
    for root in [&pair.alt, &pair.git] {
        std::fs::set_permissions(root.join(path), std::fs::Permissions::from_mode(mode)).unwrap();
    }
}

fn set(pair: &Pair, on: &str) {
    pair.both(&["config", "core.filemode", on]);
}

/// What git says has changed, one path to a line, and what alt says.
fn changed(pair: &Pair) -> (String, String) {
    let said = ok(alt(&pair.alt, &["status"]));
    (said, ok(git(&pair.git, &["status", "--porcelain"])))
}

#[test]
fn a_changed_bit_is_no_change_while_the_setting_is_off() {
    let pair = tree();
    set(&pair, "false");
    chmod(&pair, "plain", 0o755);
    chmod(&pair, "tool", 0o644);
    let (said, git_said) = changed(&pair);
    assert!(said.contains("working tree clean"), "{said}");
    assert_eq!(git_said, "");
    assert_eq!(pair.same(&["diff"]), "");
    assert_eq!(pair.same(&["diff", "--name-status", "HEAD"]), "");
    // nothing is staged of it either
    let staged = index(&pair.alt);
    pair.both(&["add", "-A"]);
    assert_eq!(index(&pair.alt), staged);
    pair.assert_same_state();
    assert!(
        staged.contains("100755") && staged.contains("100644"),
        "{staged}"
    );
}

#[test]
fn what_is_staged_keeps_the_mode_the_index_has() {
    let pair = tree();
    set(&pair, "false");
    chmod(&pair, "plain", 0o755);
    chmod(&pair, "tool", 0o644);
    pair.write("plain", "changed\n");
    pair.write("tool", "changed\n");
    chmod(&pair, "tool", 0o644);
    pair.write("new", "new\n");
    chmod(&pair, "new", 0o755);
    assert_eq!(pair.same(&["diff", "--name-status"]), "M\tplain\nM\ttool\n");
    pair.same(&["diff"]);
    pair.both(&["add", "-A"]);
    pair.assert_same_state();
    let staged = index(&pair.alt);
    let mode = |path: &str| {
        let line = staged.lines().find(|line| line.ends_with(path)).unwrap();
        line.split(' ').next().unwrap().to_owned()
    };
    // a new file is not executable, whatever its bit
    assert_eq!(
        (mode("plain"), mode("tool"), mode("new")),
        ("100644".into(), "100755".into(), "100644".into())
    );
    pair.same(&["diff", "--cached", "--name-status"]);
}

#[test]
fn the_bit_is_a_change_again_once_the_setting_is_on() {
    let pair = tree();
    set(&pair, "false");
    chmod(&pair, "plain", 0o755);
    // looked at while the setting is off, and found unchanged
    let (said, _) = changed(&pair);
    assert!(said.contains("working tree clean"), "{said}");
    set(&pair, "true");
    let (said, git_said) = changed(&pair);
    assert_eq!(git_said, " M plain\n");
    assert!(said.contains("modified:   plain"), "{said}");
    let patch = pair.same(&["diff"]);
    assert!(
        patch.contains("old mode 100644\nnew mode 100755\n"),
        "{patch}"
    );
    // and off again
    set(&pair, "false");
    let (said, git_said) = changed(&pair);
    assert!(
        said.contains("working tree clean") && git_said.is_empty(),
        "{said}{git_said}"
    );
}

#[test]
fn a_stash_and_a_reset_go_by_the_setting() {
    let pair = tree();
    set(&pair, "false");
    chmod(&pair, "plain", 0o755);
    assert_eq!(pair.same(&["stash"]), "No local changes to save\n");
    pair.write("script", "#!/bin/sh\n");
    chmod(&pair, "script", 0o755);
    pair.both(&["stash", "-u"]);
    // the untracked file was put away as a file that is not executable
    let listed = |repo: &std::path::Path| ok(git(repo, &["ls-tree", "-r", "refs/stash^3"]));
    let out = crate::like_git::export(&pair.alt);
    assert_eq!(listed(&out.path().join("exported")), listed(&pair.git));
    assert!(listed(&pair.git).starts_with("100644 "));
    pair.same(&["reset", "--hard"]);
    // the file keeps the bit it was given: nothing was there to reset
    let mode = std::fs::metadata(pair.alt.join("plain"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o111, 0o111);
}

#[test]
fn the_daemon_reads_the_setting_as_it_is_now() {
    let pair = tree();
    chmod(&pair, "plain", 0o755);
    let said = ok(alt_daemon(&pair.alt, &["status"]));
    assert!(said.contains("modified:   plain"), "{said}");
    ok(alt(&pair.alt, &["config", "core.filemode", "false"]));
    let said = ok(alt_daemon(&pair.alt, &["status"]));
    assert!(said.contains("working tree clean"), "{said}");
    assert_eq!(ok(alt_daemon(&pair.alt, &["diff"])), "");
    ok(alt(&pair.alt, &["config", "core.filemode", "true"]));
    let said = ok(alt_daemon(&pair.alt, &["status"]));
    assert!(said.contains("modified:   plain"), "{said}");
}

#[test]
fn a_parallel_workspace_goes_by_the_setting_of_the_repository() {
    let pair = tree();
    let trees = alt_os::temp::tempdir().unwrap();
    let tree = trees.path().join("ws");
    ok(alt(
        &pair.alt,
        &["workspace", "add", "ws", tree.to_str().unwrap()],
    ));
    std::fs::set_permissions(tree.join("plain"), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(ok(alt(&tree, &["status"])).contains("modified:   plain"));
    ok(alt(&pair.alt, &["config", "core.filemode", "false"]));
    assert!(ok(alt(&tree, &["status"])).contains("working tree clean"));
    ok(alt(&tree, &["add", "-A"]));
    assert_eq!(ok(alt(&tree, &["diff", "--cached"])), "");
}
