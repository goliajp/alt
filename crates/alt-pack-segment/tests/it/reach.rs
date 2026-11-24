//! Which of the commits a client names the tips it may read lead to.

use super::{git, git_in, oid, segment, text};
use alt_pack_segment::{Error, SegmentSet};
use std::fs;

#[test]
fn the_commits_said_that_the_tips_lead_to_are_told_and_no_others() {
    let work = alt_os::temp::tempdir().unwrap();
    let source = work.path().join("source");
    fs::create_dir(&source).unwrap();
    git(&source, &["init", "-q", "--initial-branch=main"]);
    let commit = |message: &str| {
        // each branch has a file of its own, and they merge
        let file = message.split(' ').next().unwrap();
        fs::write(source.join(file), message).unwrap();
        git(&source, &["add", "-A"]);
        git(&source, &["commit", "-q", "-m", message]);
        text(git(&source, &["rev-parse", "HEAD"]))
    };
    let main: Vec<String> = (0..3).map(|n| commit(&format!("main {n}"))).collect();
    // a branch that stays, and one that is taken away again
    git(&source, &["checkout", "-q", "-b", "side", &main[1]]);
    let side: Vec<String> = (0..2).map(|n| commit(&format!("side {n}"))).collect();
    git(&source, &["checkout", "-q", "-b", "gone", &main[2]]);
    let gone = commit("gone");
    git(&source, &["checkout", "-q", "main"]);
    let top: Vec<String> = (3..6).map(|n| commit(&format!("main {n}"))).collect();
    let merged = {
        git(&source, &["merge", "-q", "--no-ff", "-m", "merged", "side"]);
        text(git(&source, &["rev-parse", "HEAD"]))
    };
    let tree = text(git(&source, &["rev-parse", "HEAD^{tree}"]));
    let pack = git_in(
        &source,
        &["pack-objects", "--stdout", "-q", "--revs"],
        b"main\nside\ngone\n",
    );
    let set = SegmentSet::new(vec![segment(
        work.path(),
        "all.seg",
        &pack,
        &SegmentSet::default(),
    )]);
    let reached = |tips: &[&String], said: &[&String], steps: usize| {
        let tips: Vec<_> = tips.iter().map(|tip| oid(tip)).collect();
        let said: Vec<_> = said.iter().map(|said| oid(said)).collect();
        let reached = set.reached(&tips, &said, steps)?;
        Ok::<_, Error>(
            reached
                .iter()
                .map(alt_pack_segment::hex)
                .collect::<Vec<_>>(),
        )
    };
    let unknown = "1".repeat(40);
    // in the order they were said: what the tips lead to, a tip itself, and not
    // the commit of the branch that is gone, an id that is no commit, one not here
    let said = [&top[0], &gone, &side[0], &tree, &unknown, &merged, &main[0]];
    assert_eq!(
        reached(&[&merged, &side[1]], &said, usize::MAX).unwrap(),
        [&top[0], &side[0], &merged, &main[0]].map(String::clone)
    );
    // below a tip, not beside it and not above it
    assert_eq!(
        reached(
            &[&side[1]],
            &[&main[2], &main[1], &top[2], &side[1]],
            usize::MAX
        )
        .unwrap(),
        [&main[1], &side[1]].map(String::clone)
    );
    assert_eq!(reached(&[&merged], &[&gone], usize::MAX).unwrap(), [""; 0]);
    assert_eq!(
        reached(&[&gone], &[&gone, &main[2]], usize::MAX).unwrap(),
        [&gone, &main[2]].map(String::clone)
    );
    assert_eq!(reached(&[], &[&main[0]], usize::MAX).unwrap(), [""; 0]);
    // the walk goes no lower than the lowest commit it looks for, and no further
    // than it is given steps: from the merge to the commit before it is two
    assert_eq!(
        reached(&[&merged], &[&top[2]], 2).unwrap(),
        [top[2].clone()]
    );
    // a commit that is not led to is looked for among the five that are no lower
    assert_eq!(reached(&[&merged], &[&gone], 5).unwrap(), [""; 0]);
    assert!(matches!(
        reached(&[&merged], &[&main[0]], 3),
        Err(Error::Limit("commits of a fetch"))
    ));
}
