//! With a directory in place of an ignore file, a cherry-pick of a
//! revision that names nothing and a blame of a path that is not there
//! end on that, as they do for git, which looks at its ignore files after.

use crate::exclude_dirs::{breaks, clean_trees, each};

#[test]
fn what_is_wrong_with_the_command_itself_is_said_before_its_ignore_files() {
    let controls = [".alt/git-import", ".alt/git-import", ".git"];
    for (name, broken) in breaks() {
        let dir = alt_os::temp::tempdir().unwrap();
        let (trees, _stop) = clean_trees(dir.path());
        for (root, control) in trees.iter().zip(controls) {
            broken(root, control, true);
        }
        for wrong in [&["cherry-pick", "nosuch"][..], &["blame", "nosuch"]] {
            let (_, code, fatal) = each(&trees, wrong, wrong);
            assert_eq!((code, fatal.len()), (Some(128), 1), "{name}: {wrong:?}");
            assert!(!fatal[0].contains("exclude file"), "{name}: {fatal:?}");
        }
        // and one with nothing wrong ends on the ignore file
        let (_, _, fatal) = each(&trees, &["blame", "a.txt"], &["blame", "a.txt"]);
        assert!(fatal[0].contains("as an exclude file"), "{name}: {fatal:?}");
    }
}
