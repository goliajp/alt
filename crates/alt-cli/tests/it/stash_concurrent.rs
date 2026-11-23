//! Two workspaces that put changes away at the same moment both succeed,
//! as two git worktrees do: the stack is the repository's, and each
//! entry finds its own place on it.

use crate::like_git::{Pair, alt, ok};

#[test]
fn two_workspaces_stash_at_once_and_both_entries_are_kept() {
    let pair = Pair::new();
    pair.write("a", "a\n");
    pair.write("b", "b\n");
    pair.commit_all("base commit");
    let trees = alt_os::temp::tempdir().unwrap();
    let tree = trees.path().join("ws");
    ok(alt(
        &pair.alt,
        &["workspace", "add", "ws", tree.to_str().unwrap()],
    ));
    const ROUNDS: usize = 12;
    for round in 0..ROUNDS {
        std::fs::write(tree.join("a"), format!("ws {round}\n")).unwrap();
        std::fs::write(pair.alt.join("b"), format!("default {round}\n")).unwrap();
        let go = std::sync::Barrier::new(2);
        let stash = |root: &std::path::Path| {
            go.wait();
            alt(root, &["stash"])
        };
        let (first, second) = std::thread::scope(|scope| {
            let first = scope.spawn(|| stash(&tree));
            let second = scope.spawn(|| stash(&pair.alt));
            (first.join().unwrap(), second.join().unwrap())
        });
        ok(first);
        ok(second);
        let listed = ok(alt(&pair.alt, &["stash", "list"]));
        assert_eq!(listed.lines().count(), 2 * (round + 1), "{listed}");
        assert_eq!(std::fs::read_to_string(tree.join("a")).unwrap(), "a\n");
        assert_eq!(std::fs::read_to_string(pair.alt.join("b")).unwrap(), "b\n");
    }
}
