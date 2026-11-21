//! The paths offered to the commands that take them.

use super::{alt, alt_with, answer, group, groups, repository};

#[test]
fn paths_are_offered_by_what_the_command_does_with_them() {
    let dir = repository();
    let root = dir.path();
    assert_eq!(
        answer(root, "alt add "),
        "group\tmodified-files\tmodified file\na.txt\t\n\
         group\tuntracked-files\tuntracked file\nnew.txt\t\n\
         group\tuntracked-files\tuntracked file\nfresh/\t\nnospace\n"
    );
    // a path already on the line is not offered again
    let again = answer(root, "alt add new.txt ");
    assert_eq!(group(&again, "untracked-files"), ["fresh/"]);
    // paths are named from where the command is run
    let within = answer(&root.join("src"), "alt add ");
    assert_eq!(group(&within, "modified-files"), ["../a.txt"]);
    assert_eq!(
        answer(root, "alt blame "),
        "group\ttracked-files\ttracked file\na.txt\t\nsrc/lib.rs\t\n"
    );
}

#[test]
fn paths_in_conflict_come_first() {
    let dir = repository();
    let root = dir.path();
    alt(root, &["add", "a.txt"]);
    alt(root, &["commit", "-m", "ours"]);
    alt(root, &["switch", "topic"]);
    std::fs::write(root.join("a.txt"), "theirs\n").unwrap();
    alt(root, &["add", "a.txt"]);
    alt(root, &["commit", "-m", "theirs"]);
    let merged = alt_with(root, &["merge", "main"], false);
    assert_eq!(merged.status.code(), Some(1));
    let add = answer(root, "alt add ");
    assert_eq!(
        groups(&add),
        ["unmerged-files", "untracked-files", "untracked-files"]
    );
    assert_eq!(group(&add, "unmerged-files"), ["a.txt"]);
}

#[test]
fn many_paths_are_offered_a_directory_at_a_time() {
    let dir = alt_os::temp::tempdir().unwrap();
    let root = dir.path();
    alt(root, &["init", "."]);
    std::fs::create_dir_all(root.join("gen/deep")).unwrap();
    for number in 0..1001 {
        std::fs::write(root.join(format!("gen/f{number:04}")), "x\n").unwrap();
    }
    std::fs::write(root.join("gen/deep/y"), "y\n").unwrap();
    std::fs::write(root.join("top"), "t\n").unwrap();
    alt(root, &["add", "."]);
    alt(root, &["commit", "-m", "many"]);
    assert_eq!(
        answer(root, "alt blame "),
        "group\ttracked-files\ttracked file\ntop\t\n\
         group\ttracked-files\ttracked file\ngen/\t\nnospace\n"
    );
    let level = answer(root, "alt blame gen/f000");
    let files = group(&level, "tracked-files");
    assert_eq!(files.len(), 10, "{level}");
    assert!(files.iter().all(|path| path.starts_with("gen/f000")));
}
