//! what a fetch selects when the client holds part of the history

use super::{git, git_in, objects, oid, segment, text};
use alt_pack_segment::SegmentSet;
use std::{collections::HashSet, fs, path::Path};

fn write(dir: &Path, path: &str, body: &[u8]) {
    let path = dir.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

fn commit(dir: &Path, message: &str) -> String {
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", message]);
    text(git(dir, &["rev-parse", "HEAD"]))
}

/// bytes that do not compress, so that the blob stays large in the pack too
fn noise(len: usize) -> Vec<u8> {
    let mut state = 0x2545_f491_4f6c_dd1du64;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect()
}

/// Selects `wants` for a client that holds `haves`, checks the selection against what
/// Git itself would send, and applies the pack to a repository holding only `haves`.
/// Returns the objects selected beyond Git's answer.
fn fetch(dir: &Path, set: &SegmentSet, wants: &[&str], haves: &[&str], name: &str) -> Vec<String> {
    let mut range: Vec<String> = wants.iter().map(|want| want.to_string()).collect();
    range.extend(haves.iter().map(|have| format!("^{have}")));
    let range: Vec<&str> = range.iter().map(String::as_str).collect();
    let expected: HashSet<String> = objects(dir, &range).into_iter().collect();
    let wants_raw: Vec<_> = wants.iter().map(|want| oid(want)).collect();
    let haves_raw: Vec<_> = haves.iter().map(|have| oid(have)).collect();
    let selection = set.select(&wants_raw, &haves_raw).unwrap();
    let selected: HashSet<String> = selection
        .objects()
        .iter()
        .map(alt_pack_segment::hex)
        .collect();
    assert_eq!(selected.len(), selection.len(), "an object selected twice");
    let missing: Vec<_> = expected.difference(&selected).collect();
    assert!(missing.is_empty(), "{name}: not selected: {missing:?}");

    let target = dir.parent().unwrap().join(name);
    fs::create_dir(&target).unwrap();
    git(&target, &["init", "-q", "--bare"]);
    if !haves.is_empty() {
        let held = git_in(
            dir,
            &["pack-objects", "--stdout", "-q", "--revs"],
            (haves.join("\n") + "\n").as_bytes(),
        );
        git_in(&target, &["index-pack", "--stdin", "--strict"], &held);
    }
    let mut sent = Vec::new();
    set.write_selected(&selection, true, &mut sent).unwrap();
    git_in(
        &target,
        &["index-pack", "--stdin", "--strict", "--fix-thin"],
        &sent,
    );
    for (index, want) in wants.iter().enumerate() {
        git(
            &target,
            &["update-ref", &format!("refs/heads/w{index}"), want],
        );
    }
    git(&target, &["fsck", "--strict", "--full"]);
    let mut extra: Vec<String> = selected
        .difference(&expected)
        .map(|id| format!("{id} {}", text(git(dir, &["cat-file", "-t", id]))))
        .collect();
    extra.sort();
    extra
}

#[test]
fn a_fetch_sends_what_changed_and_nothing_a_changed_directory_already_held() {
    let work = alt_os::temp::tempdir().unwrap();
    let source = work.path().join("source");
    fs::create_dir(&source).unwrap();
    git(&source, &["init", "-q", "--initial-branch=main"]);
    write(&source, "a/x.txt", b"x one\n");
    write(&source, "a/keep.txt", b"kept\n");
    write(&source, "b/y.txt", b"moves later\n");
    write(&source, "c/small.txt", b"copied later\n");
    write(&source, "c/z.txt", b"z\n");
    write(&source, "large/big.bin", &noise(300 * 1024));
    write(&source, "deep/d1/d2/f.txt", b"deep one\n");
    let held = commit(&source, "held by the client");

    // a commit adds a file, the next one changes its sibling: the added file is in
    // the base of the second comparison and still has to be sent
    write(&source, "a/new.txt", b"added\n");
    let added = commit(&source, "add a file");
    write(&source, "a/x.txt", b"x two\n");
    fs::create_dir(source.join("moved")).unwrap();
    git(&source, &["mv", "b/y.txt", "moved/y.txt"]);
    let changed = commit(
        &source,
        "change its sibling, move a file to another directory",
    );
    write(&source, "copy/small.txt", b"copied later\n");
    let copied = commit(&source, "copy a small file out of an untouched directory");

    git(&source, &["checkout", "-q", "-b", "side", &held]);
    write(&source, "deep/d1/d2/f.txt", b"deep two\n");
    let side = commit(&source, "side branch");
    git(&source, &["checkout", "-q", "main"]);
    git(&source, &["merge", "-q", "--no-ff", "-m", "merge", "side"]);
    let merged = text(git(&source, &["rev-parse", "HEAD"]));
    write(&source, "copy/big.bin", &noise(300 * 1024));
    let large = commit(&source, "copy a large file out of an untouched directory");

    let pack = git_in(
        &source,
        &[
            "pack-objects",
            "--all",
            "--stdout",
            "-q",
            "--delta-base-offset",
        ],
        b"",
    );
    let set = SegmentSet::new(vec![segment(
        work.path(),
        "all.seg",
        &pack,
        &SegmentSet::default(),
    )]);
    let blob = |path: &str| text(git(&source, &["rev-parse", &format!("{large}:{path}")]));

    assert_eq!(
        fetch(&source, &set, &[&added], &[&held], "one"),
        Vec::<String>::new()
    );
    // the moved file left a directory that changed, so it is known to be held
    assert_eq!(
        fetch(&source, &set, &[&changed], &[&held], "two"),
        Vec::<String>::new()
    );
    assert_eq!(
        fetch(&source, &set, &[&changed], &[&added], "three"),
        Vec::<String>::new()
    );
    // the copy's source directory is never opened; a small blob is sent again
    assert_eq!(
        fetch(&source, &set, &[&copied], &[&held], "four"),
        [blob("c/small.txt") + " blob"]
    );
    assert_eq!(
        fetch(&source, &set, &[&merged], &[&held, &side], "five"),
        [blob("c/small.txt") + " blob"]
    );
    assert_eq!(
        fetch(&source, &set, &[&merged], &[&copied], "six"),
        Vec::<String>::new()
    );
    // a large blob is worth the exact answer, which then covers the small copy too
    assert_eq!(
        fetch(&source, &set, &[&large], &[&held], "seven"),
        Vec::<String>::new()
    );
    assert_eq!(
        fetch(&source, &set, &[&large], &[&merged], "eight"),
        Vec::<String>::new()
    );
    // nothing held: everything
    assert_eq!(
        fetch(&source, &set, &[&large], &[], "nine"),
        Vec::<String>::new()
    );
    assert_eq!(
        fetch(&source, &set, &[&large, &side], &[&added], "ten"),
        Vec::<String>::new()
    );
}

/// A directory that returns to an earlier state is compared in both directions,
/// first against the state between and then that state against it. Neither comparison
/// covers what the two states share; only the commit that created the earlier state does.
#[test]
fn a_directory_that_returns_to_an_earlier_state_is_sent_whole() {
    let work = alt_os::temp::tempdir().unwrap();
    let source = work.path().join("source");
    fs::create_dir(&source).unwrap();
    git(&source, &["init", "-q", "--initial-branch=main"]);
    write(&source, "top.txt", b"top\n");
    let root = commit(&source, "root");
    write(&source, "dir/kept.txt", b"shared by both states\n");
    write(&source, "dir/sub/deep.txt", b"shared, one level down\n");
    write(&source, "dir/flip.txt", b"first\n");
    let created = commit(&source, "create the directory");
    write(&source, "dir/flip.txt", b"second\n");
    commit(&source, "change one file");
    write(&source, "dir/flip.txt", b"first\n");
    let back = commit(&source, "change it back");

    let pack = git_in(&source, &["pack-objects", "--all", "--stdout", "-q"], b"");
    let set = SegmentSet::new(vec![segment(
        work.path(),
        "all.seg",
        &pack,
        &SegmentSet::default(),
    )]);
    assert_eq!(
        fetch(&source, &set, &[&back], &[], "clone"),
        Vec::<String>::new()
    );
    assert_eq!(
        fetch(&source, &set, &[&back], &[&root], "from the root"),
        Vec::<String>::new()
    );
    assert_eq!(
        fetch(&source, &set, &[&back], &[&created], "from the first state"),
        Vec::<String>::new()
    );
}

/// A file gives way to a directory of its name, and the directory to a file again:
/// what is at a name is compared with what was there only where both are of one kind.
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
    let file = commit(&source, "a file");
    fs::remove_file(source.join("thing")).unwrap();
    write(&source, "thing/inside.txt", b"in the directory\n");
    let directory = commit(&source, "a directory of its name");
    write(&source, "thing/inside.txt", b"changed\n");
    let changed = commit(&source, "change in the directory");
    fs::remove_dir_all(source.join("thing")).unwrap();
    write(&source, "thing", b"a file again\n");
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

/// More versions of one directory than a walk keeps at once: those it let go of are
/// made again, and the selection and the pack are what they are for a short history.
#[test]
fn a_history_of_more_trees_than_a_walk_keeps_is_selected_whole() {
    let work = alt_os::temp::tempdir().unwrap();
    let source = work.path().join("source");
    fs::create_dir(&source).unwrap();
    git(&source, &["init", "-q", "--initial-branch=main"]);
    // a directory of some eighty kilobytes, in three hundred versions
    for file in 0..2000 {
        write(
            &source,
            &format!("wide/file-with-a-long-name-{file:05}.txt"),
            b"0\n",
        );
    }
    let mut commits = vec![commit(&source, "wide")];
    for version in 1..300usize {
        let file = version * 7 % 2000;
        write(
            &source,
            &format!("wide/file-with-a-long-name-{file:05}.txt"),
            format!("{version}\n").as_bytes(),
        );
        commits.push(commit(&source, &format!("version {version}")));
    }
    git(&source, &["repack", "-adq", "--depth=50", "--window=10"]);
    let pack = git_in(
        &source,
        &[
            "pack-objects",
            "--all",
            "--stdout",
            "-q",
            "--delta-base-offset",
        ],
        b"",
    );
    let set = SegmentSet::new(vec![segment(
        work.path(),
        "all.seg",
        &pack,
        &SegmentSet::default(),
    )]);
    let none = Vec::<String>::new();
    let tip = commits.last().unwrap();
    assert_eq!(fetch(&source, &set, &[tip], &[], "clone"), none);
    assert_eq!(fetch(&source, &set, &[tip], &[&commits[150]], "half"), none);
    assert_eq!(
        fetch(&source, &set, &[&commits[150]], &[&commits[3]], "middle"),
        none
    );
}
