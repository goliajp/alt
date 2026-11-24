//! A checkout that moves every file of a directory to another renames
//! the directory, and leaves in place what is no file of the checkout.

use crate::like_git::Pair;

#[test]
fn a_directory_that_moves_whole_leaves_an_empty_directory_where_git_leaves_it() {
    let pair = Pair::new();
    pair.write("d/x", "x\n");
    pair.write("d/s/y", "y\n");
    pair.write("keep", "k\n");
    pair.commit_all("base");
    pair.both(&["switch", "-c", "moved"]);
    pair.both(&["mv", "d", "e"]);
    pair.commit_all("moved");
    pair.both(&["switch", "main"]);
    let same_directories = |when: &str| {
        for dir in [
            "d",
            "d/empty",
            "d/s",
            "d/s/hollow",
            "e",
            "e/empty",
            "e/s",
            "e/s/hollow",
        ] {
            let (ours, theirs) = (pair.alt.join(dir).is_dir(), pair.git.join(dir).is_dir());
            assert_eq!(ours, theirs, "{dir} {when}");
        }
    };
    // directories no file is in, which are nothing the checkout tracks
    for root in [&pair.alt, &pair.git] {
        std::fs::create_dir_all(root.join("d/empty")).unwrap();
        std::fs::create_dir_all(root.join("d/s/hollow")).unwrap();
    }
    pair.both(&["switch", "moved"]);
    pair.assert_same_state();
    same_directories("moved");
    assert!(pair.git.join("d/empty").is_dir() && !pair.git.join("e/empty").exists());
    pair.both(&["switch", "main"]);
    pair.assert_same_state();
    same_directories("back");
}
