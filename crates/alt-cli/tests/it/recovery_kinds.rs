//! Recovery of a checkout interrupted while a path changes between a
//! directory, a file and a submodule.

use std::path::Path;
use std::process::Command;

use super::undo_add::{
    alt, index_bytes, ok, recover_pending, remove_final_log_frame, staged_paths,
};

/// Switches from `main` to `other`, drops the record that the checkout
/// completed, puts the working tree into `midway` — a state the checkout
/// passes through — and recovers. The checkout must then be complete.
fn recovers_from(root: &Path, midway: &dyn Fn(&Path), complete: &dyn Fn(&Path)) {
    let index_before = index_bytes(root);
    ok(alt(root, &["switch", "other"]));
    remove_final_log_frame(root);
    std::fs::write(root.join(".alt/index"), index_before).unwrap();
    midway(root);
    recover_pending(root);
    complete(root);
    let status: alt_json::Value = alt_json::from_str(&staged_paths(root)).unwrap();
    assert_eq!(status["staged"], alt_json::json!([]));
    assert_eq!(status["unstaged"], alt_json::json!([]));
}

#[test]
fn recovery_finishes_a_checkout_that_turns_a_directory_into_a_file_or_back() {
    let put = |root: &Path, rel: &str, text: &str| {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    };
    let tree = |root: &Path| {
        put(root, "node/a.txt", "a");
        put(root, "node/deep/b.txt", "b");
    };
    let flat = |root: &Path| put(root, "node", "flat");
    let is_tree = |root: &Path| {
        assert_eq!(std::fs::read(root.join("node/a.txt")).unwrap(), b"a");
        assert_eq!(std::fs::read(root.join("node/deep/b.txt")).unwrap(), b"b");
    };
    let is_flat = |root: &Path| assert_eq!(std::fs::read(root.join("node")).unwrap(), b"flat");
    // each state the checkout passes through, in both directions
    let wipe = |root: &Path| {
        let node = root.join("node");
        match node.is_dir() {
            true => std::fs::remove_dir_all(node).unwrap(),
            false => drop(std::fs::remove_file(node)),
        }
    };
    type Step<'a> = &'a dyn Fn(&Path);
    let to_flat: [Step<'_>; 3] = [
        &|root| {
            wipe(root);
            tree(root);
        },
        &|root| {
            wipe(root);
            put(root, "node/deep/b.txt", "b");
        },
        &|root| wipe(root),
    ];
    let to_tree: [Step<'_>; 3] = [
        &|root| {
            wipe(root);
            flat(root);
        },
        &|root| wipe(root),
        &|root| {
            wipe(root);
            put(root, "node/a.txt", "a");
        },
    ];
    for (first, second, steps, complete) in [
        (
            &tree as Step<'_>,
            &flat as Step<'_>,
            to_flat,
            &is_flat as Step<'_>,
        ),
        (&flat, &tree, to_tree, &is_tree),
    ] {
        for midway in steps {
            let dir = alt_os::temp::tempdir().unwrap();
            let root = dir.path();
            ok(alt(root, &["init", "."]));
            put(root, "keep", "k");
            first(root);
            ok(alt(root, &["add", "."]));
            ok(alt(root, &["commit", "-m", "first"]));
            ok(alt(root, &["switch", "-c", "other"]));
            wipe(root);
            second(root);
            ok(alt(root, &["add", "."]));
            ok(alt(root, &["commit", "-m", "second"]));
            ok(alt(root, &["switch", "main"]));
            recovers_from(root, midway, complete);
        }
    }
}

#[test]
fn recovery_finishes_a_checkout_that_swaps_a_file_and_a_submodule() {
    let git = |repo: &Path, args: &[&str]| {
        let out = Command::new("git")
            .arg("-C")
            .arg(repo)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "tester")
            .env("GIT_AUTHOR_EMAIL", "t@e")
            .env("GIT_COMMITTER_NAME", "tester")
            .env("GIT_COMMITTER_EMAIL", "t@e")
            .args(args)
            .output()
            .unwrap();
        ok(out)
    };
    let link = "160000,1111111111111111111111111111111111111111,sub";
    for file_first in [true, false] {
        let (src, dst) = (
            alt_os::temp::tempdir().unwrap(),
            alt_os::temp::tempdir().unwrap(),
        );
        let (src, root) = (src.path(), dst.path());
        git(src, &["init", "-q", "-b", "main", "."]);
        std::fs::write(src.join("keep"), "k").unwrap();
        let as_file = |src: &Path| {
            std::fs::write(src.join("sub"), "a file").unwrap();
            git(src, &["add", "."]);
        };
        let as_link = |src: &Path| {
            git(src, &["add", "."]);
            git(src, &["update-index", "--add", "--cacheinfo", link]);
        };
        match file_first {
            true => as_file(src),
            false => as_link(src),
        }
        git(src, &["commit", "-qm", "first"]);
        git(src, &["branch", "other"]);
        git(src, &["rm", "-q", "--cached", "sub"]);
        let _ = std::fs::remove_file(src.join("sub"));
        match file_first {
            true => as_link(src),
            false => as_file(src),
        }
        git(src, &["commit", "-qm", "second"]);
        git(src, &["branch", "-f", "other", "HEAD"]);
        git(src, &["reset", "-q", "--hard", "HEAD~1"]);
        ok(alt(src, &["import", root.to_str().unwrap()]));

        // midway the path is gone: the old kind removed, the new not made
        let gone = |root: &Path| {
            let sub = root.join("sub");
            match sub.is_dir() {
                true => std::fs::remove_dir(sub).unwrap(),
                false => std::fs::remove_file(sub).unwrap(),
            }
        };
        let complete = |root: &Path| match file_first {
            true => assert!(root.join("sub").is_dir()),
            false => assert_eq!(std::fs::read(root.join("sub")).unwrap(), b"a file"),
        };
        recovers_from(root, &gone, &complete);
    }
}
