//! Trees as Git takes them and does not write them: a mode with a zero before
//! it, and entries out of their order.

use super::{git, git_in, oid, segment, text};
use alt_pack_segment::SegmentSet;
use std::fs;

fn tree(dir: &std::path::Path, entries: &[(&str, &str, &str)]) -> String {
    let mut data = Vec::new();
    for (mode, name, id) in entries {
        data.extend_from_slice(format!("{mode} {name}\0").as_bytes());
        data.extend_from_slice(&oid(id));
    }
    let args = ["hash-object", "-t", "tree", "-w", "--literally", "--stdin"];
    text(git_in(dir, &args, &data))
}

/// A directory whose mode is written `040000`, and a tree whose entries are not
/// in the order Git writes: what is under them is sent with a clone, and with
/// a fetch behind a commit that has other content there.
#[test]
fn what_is_under_a_directory_of_a_padded_mode_and_in_a_tree_out_of_order_is_sent() {
    let work = alt_os::temp::tempdir().unwrap();
    let source = work.path().join("source");
    fs::create_dir(&source).unwrap();
    git(&source, &["init", "-q", "--initial-branch=main"]);
    let blob = |content: &str| {
        let args = ["hash-object", "-w", "--stdin"];
        text(git_in(&source, &args, content.as_bytes()))
    };
    let commit = |tree: &str, parents: &[&str]| {
        let mut args = vec!["commit-tree", tree, "-m", "made by hand"];
        for parent in parents {
            args.extend(["-p", parent]);
        }
        text(git(&source, &args))
    };
    let (one, two, three) = (blob("one\n"), blob("two\n"), blob("three\n"));
    let inner = tree(&source, &[("100644", "file", &one)]);
    let padded = tree(
        &source,
        &[("040000", "dir", &inner), ("0100644", "beside", &two)],
    );
    let first = commit(&padded, &[]);
    // the entries of the second in no order, a directory among them
    let other = tree(&source, &[("100644", "file", &three)]);
    let unordered = tree(
        &source,
        &[
            ("100644", "z", &two),
            ("040000", "dir", &other),
            ("100644", "a", &one),
        ],
    );
    let second = commit(&unordered, &[&first]);
    git(&source, &["update-ref", "refs/heads/main", &second]);
    let pack = git_in(&source, &["pack-objects", "--all", "--stdout", "-q"], b"");
    let set = SegmentSet::new(vec![segment(
        work.path(),
        "one.seg",
        &pack,
        &SegmentSet::default(),
    )]);
    let sent = |wants: &str, haves: &[[u8; 20]], names: &[&String]| {
        let selection = set.select(&[oid(wants)], haves).unwrap();
        let mut pack = Vec::new();
        set.write_selected(&selection, true, &mut pack).unwrap();
        let target = work.path().join(format!("target-{}", haves.len()));
        fs::create_dir(&target).unwrap();
        git(&target, &["init", "-q", "--bare"]);
        git_in(&target, &["index-pack", "--stdin", "--fix-thin"], &pack);
        for name in names {
            git(&target, &["cat-file", "-e", name]);
        }
        selection.len()
    };
    let of_first = [&first, &padded, &inner, &one, &two];
    assert_eq!(sent(&first, &[], &of_first), of_first.len());
    // behind the first: the directory changed, and so did what is in it
    let of_second = [&second, &unordered, &other, &three];
    assert!(sent(&second, &[oid(&first)], &of_second) >= of_second.len());
}
