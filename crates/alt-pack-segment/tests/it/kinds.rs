//! what a fetch selects where a name is a file in one commit and a directory in another

use super::fetch::{commit, fetch, write};
use super::{git, git_in, segment};
use alt_pack_segment::SegmentSet;
use std::fs;

/// A file gives way to a directory of its name, and the directory to a file again:
/// what is at a name is compared with what was there only where both are of one kind.
/// Once among names that stand between the two in a tree, and once alone in its
/// directory, where the file and the directory are met at the same step.
#[test]
fn a_file_and_a_directory_of_one_name_are_not_compared_with_each_other() {
    let work = alt_os::temp::tempdir().unwrap();
    let source = work.path().join("source");
    fs::create_dir(&source).unwrap();
    git(&source, &["init", "-q", "--initial-branch=main"]);
    write(&source, "thing", b"a file\n");
    write(
        &source,
        "thing-x",
        b"before the directory, after the file\n",
    );
    write(&source, "thing.d", b"as well\n");
    write(&source, "alone/thing", b"a file by itself\n");
    let file = commit(&source, "a file");
    for thing in ["thing", "alone/thing"] {
        fs::remove_file(source.join(thing)).unwrap();
        write(
            &source,
            &format!("{thing}/inside.txt"),
            b"in the directory\n",
        );
    }
    let directory = commit(&source, "a directory of its name");
    write(&source, "thing/inside.txt", b"changed\n");
    write(&source, "alone/thing/inside.txt", b"changed too\n");
    let changed = commit(&source, "change in the directory");
    for thing in ["thing", "alone/thing"] {
        fs::remove_dir_all(source.join(thing)).unwrap();
        write(&source, thing, b"a file again\n");
    }
    let again = commit(&source, "a file again");

    let pack = git_in(&source, &["pack-objects", "--all", "--stdout", "-q"], b"");
    let set = SegmentSet::new(vec![segment(
        work.path(),
        "all.seg",
        &pack,
        &SegmentSet::default(),
    )]);
    let none = Vec::<String>::new();
    assert_eq!(fetch(&source, &set, &[&again], &[], "clone"), none);
    assert_eq!(fetch(&source, &set, &[&directory], &[&file], "to it"), none);
    assert_eq!(fetch(&source, &set, &[&changed], &[&file], "past it"), none);
    assert_eq!(fetch(&source, &set, &[&again], &[&changed], "back"), none);
    assert_eq!(fetch(&source, &set, &[&again], &[&file], "over"), none);
}
