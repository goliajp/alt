//! The daemon's answers also go by what lies outside the working tree:
//! the ignore files, the user's and the repository's, the attributes and
//! the settings. A change to any of them reaches the next command.

use crate::scan_daemon::{agree, run, served, watches, write};

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

#[test]
fn attributes_and_settings_that_change_how_a_file_compares_reach_the_daemon() {
    let (dir, _stop) = served();
    let root = dir.path();
    write(root, "a.txt", "aaaa\r\n");
    write(root, "src/lib.rs", "lib\r\n");
    let status = agree(root, "other line endings");
    assert!(
        status.contains("modified:   a.txt") && status.contains("modified:   src/lib.rs"),
        "{status}"
    );
    write(root, "src/.gitattributes", "*.rs text\n");
    let status = agree(root, "attributes in a directory");
    assert!(
        status.contains("modified:   a.txt") && !status.contains("src/lib.rs"),
        "{status}"
    );
    write(root, ".gitattributes", "* text=auto\n");
    let status = agree(root, "attributes at the root");
    assert!(!status.contains("modified:"), "{status}");
    // a file found unchanged stays so until it is written again
    std::fs::remove_file(root.join(".gitattributes")).unwrap();
    std::fs::remove_file(root.join("src/.gitattributes")).unwrap();
    assert!(agree(root, "attributes removed").contains("nothing to commit"));
    write(root, "a.txt", "AAAA\r\n");
    assert!(agree(root, "written again").contains("modified:   a.txt"));
    write(root, ".gitattributes", "* text=auto\n");
    assert!(agree(root, "attributes again").contains("modified:   a.txt"));
    write(root, "a.txt", "aaaa\r\n");
    assert!(!agree(root, "as stored but for line endings").contains("modified:"));
    std::fs::remove_file(root.join(".gitattributes")).unwrap();

    // the repository's settings are no part of the tree
    let config = root.join(".alt/git-import/config");
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    for setting in ["true", "input"] {
        write(root, "a.txt", "aaaa\r\n");
        assert!(agree(root, "no settings").contains("modified:   a.txt"));
        std::fs::write(&config, format!("[core]\n\tautocrlf = {setting}\n")).unwrap();
        let status = agree(root, setting);
        assert!(status.contains("nothing to commit"), "{setting}: {status}");
        std::fs::remove_file(&config).unwrap();
    }
}

#[test]
fn every_file_the_settings_and_attributes_come_from_reaches_the_daemon() {
    let (dir, _stop) = served();
    let root = dir.path();
    let import = root.join(".alt/git-import");
    std::fs::create_dir_all(import.join("info")).unwrap();
    // stored with LF: modified until something says the file is text
    let crlf = |at: &str| {
        write(root, "a.txt", "aaaa\r\n");
        assert!(agree(root, at).contains("modified:   a.txt"), "{at}");
    };
    let clean = |at: &str| {
        let status = agree(root, at);
        assert!(status.contains("nothing to commit"), "{at}: {status}");
    };

    crlf("no attributes");
    let info = import.join("info/attributes");
    std::fs::write(&info, "* text\n").unwrap();
    clean("the repository's attributes");
    std::fs::remove_file(&info).unwrap();

    crlf("no attributes of the user");
    let user = root.join(".alt/test-config/git/attributes");
    std::fs::create_dir_all(user.parent().unwrap()).unwrap();
    std::fs::write(&user, "* text\n").unwrap();
    clean("the user's attributes");
    std::fs::remove_file(&user).unwrap();

    // an included file that is not there yet, then is
    let config = import.join("config");
    std::fs::write(&config, "[include]\n\tpath = extra.cfg\n").unwrap();
    crlf("nothing included");
    std::fs::write(import.join("extra.cfg"), "[core]\n\tautocrlf = true\n").unwrap();
    clean("an included file");
    std::fs::remove_file(import.join("extra.cfg")).unwrap();

    // a file included on one branch only: neither it nor the index changes
    let on_side = "[includeIf \"onbranch:side\"]\n\tpath = side.cfg\n";
    std::fs::write(&config, on_side).unwrap();
    std::fs::write(import.join("side.cfg"), "[core]\n\tautocrlf = true\n").unwrap();
    crlf("on a branch that includes nothing");
    run(root, &["switch", "-c", "side"], true);
    clean("on the branch that includes it");
    run(root, &["switch", "main"], true);
    crlf("back on a branch that includes nothing");
}
