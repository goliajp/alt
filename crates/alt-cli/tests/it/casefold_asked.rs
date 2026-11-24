//! A directory in a parallel workspace whose name the file system takes
//! for that of the directory alt asks it in, `.alt-names-asked`, written
//! in another case: it is somebody's, and no command clears it away.

use crate::casefold::{code, place};
use crate::like_git::{alt, ok};

#[test]
#[ignore = "needs ALT_CASEFOLD_DIR: a directory on a file system that takes names differing in case for one"]
fn a_directory_spelled_like_the_one_alt_asks_in_is_not_cleared_away() {
    let held = alt_os::temp::tempdir_in(place()).unwrap();
    let repo = held.path().join("r");
    std::fs::create_dir(&repo).unwrap();
    ok(alt(&repo, &["init", "."]));
    std::fs::write(repo.join("f"), "f\n").unwrap();
    ok(alt(&repo, &["add", "f"]));
    ok(alt(&repo, &["commit", "-m", "one"]));
    // a branch that brings two names into one directory: they are asked about
    ok(alt(&repo, &["switch", "-c", "two"]));
    std::fs::write(repo.join("p"), "p\n").unwrap();
    std::fs::write(repo.join("q"), "q\n").unwrap();
    ok(alt(&repo, &["add", "-A"]));
    ok(alt(&repo, &["commit", "-m", "two"]));
    ok(alt(&repo, &["switch", "main"]));
    ok(alt(&repo, &["branch", "side", "main"]));
    let tree = held.path().join("tree");
    let add = ["workspace", "add", "ws", tree.to_str().unwrap(), "side"];
    ok(alt(&repo, &add));
    let theirs = tree.join(".ALT-NAMES-ASKED");
    std::fs::create_dir(&theirs).unwrap();
    std::fs::write(theirs.join("mine"), "mine\n").unwrap();
    let kept = || {
        let names = std::fs::read_dir(&tree).unwrap();
        let names: Vec<_> = names.map(|e| e.unwrap().file_name()).collect();
        assert!(names.contains(&".ALT-NAMES-ASKED".into()), "{names:?}");
        assert_eq!(
            std::fs::read_to_string(theirs.join("mine")).unwrap(),
            "mine\n"
        );
    };

    // a command that writes, though it asks nothing
    std::fs::write(tree.join("g"), "g\n").unwrap();
    ok(alt(&tree, &["add", "g"]));
    kept();
    ok(alt(&tree, &["commit", "-m", "g"]));
    kept();
    // one that asks is refused before it changes anything
    let refused = alt(&tree, &["merge", "two"]);
    let said = String::from_utf8_lossy(&refused.stderr).into_owned();
    assert!(!refused.status.success(), "{said}");
    assert!(said.contains("is taken here by a name"), "{said}");
    kept();
    assert_eq!(ok(alt(&tree, &["operation", "pending"])), "");
    assert!(!tree.join("q").exists());
    // and no path is staged under the name
    let named = alt(&tree, &["add", ".ALT-NAMES-ASKED/mine"]);
    let said = String::from_utf8_lossy(&named.stderr).into_owned();
    assert_eq!(code(&named), Some(128), "{said}");
    assert!(said.contains("invalid path"), "{said}");
    kept();
    // once it is out of the way the merge is made
    std::fs::rename(&theirs, held.path().join("moved")).unwrap();
    ok(alt(&tree, &["merge", "two"]));
    assert_eq!(std::fs::read_to_string(tree.join("q")).unwrap(), "q\n");
    assert!(!tree.join(".alt-names-asked").exists());
}
