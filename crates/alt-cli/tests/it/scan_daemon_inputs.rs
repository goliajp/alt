//! The daemon's answers also go by the ignore files outside the working
//! tree, the user's and the repository's. A change to either reaches the
//! next command.

use crate::scan_daemon::{agree, served, watches, write};

#[test]
fn ignore_files_outside_the_tree_reach_the_daemon() {
    let (dir, stop) = served();
    let root = dir.path();
    write(root, "out/deep/a.o", "o\n");
    write(root, "notes.tmp", "n\n");
    let shows = |at: &str, out: bool, notes: bool| {
        let status = agree(root, at);
        assert_eq!(status.contains("\tout/\n"), out, "{at}: {status}");
        assert_eq!(status.contains("\tnotes.tmp\n"), notes, "{at}: {status}");
    };
    shows("nothing hidden", true, true);
    // the root, docs, src, src/deep, out, out/deep
    assert_eq!(watches(stop.0), (1, 6));

    // the repository's own file; what it hides is not watched
    write(root, ".alt/git-import/info/exclude", "/out/\n");
    shows("the repository's file", false, true);
    assert_eq!(watches(stop.0), (1, 4));
    // the user's, where it is looked for when no setting names one
    write(root, ".alt/test-config/git/ignore", "*.tmp\n");
    shows("the user's file", false, false);
    // a file in the tree is above both
    write(root, "src/.gitignore", "!*.tmp\n");
    write(root, "src/more.tmp", "m\n");
    assert!(agree(root, "a file in the tree").contains("\tsrc/more.tmp\n"));
    std::fs::remove_file(root.join("src/more.tmp")).unwrap();
    std::fs::remove_file(root.join("src/.gitignore")).unwrap();

    // a setting names another file, which is not there yet
    let named = root.join(".alt/named-rules");
    let config = format!("[core]\n\texcludesFile = {}\n", named.display());
    write(root, ".alt/git-import/config", &config);
    shows("a setting that names no file yet", false, true);
    std::fs::write(&named, "*.tmp\n!/out/\n").unwrap();
    shows("the named file", false, false);
    std::fs::write(&named, "*.o\n").unwrap();
    shows("the named file with other rules", false, true);
    std::fs::remove_file(root.join(".alt/git-import/info/exclude")).unwrap();
    shows("only the named file", false, true);
    std::fs::remove_file(root.join(".alt/git-import/config")).unwrap();
    shows("the user's file again", true, false);
    assert_eq!(watches(stop.0), (1, 6));
}
