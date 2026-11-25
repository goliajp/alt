//! What tips lead to, found from what earlier tips led to.

use super::{git, git_in, objects, oid, segment, text};
use alt_pack_segment::{Error, Led, SegmentSet};
use std::fs;

/// `led` is exactly the objects Git lists for `tips`
fn same(source: &std::path::Path, led: &Led, tips: &[&str]) {
    let listed = objects(source, tips);
    for id in &listed {
        assert!(led.to(&oid(id)), "{id} of {tips:?}");
    }
    assert_eq!(led.len(), listed.len(), "{tips:?}");
}

/// A history of directories that change one at a time, in two segments, with a
/// branch beside it and a tag on it. What its tips lead to is what Git lists
/// for them, found from nothing or from what earlier tips led to; found from
/// those it reads the trees that are new and no others; and a walk that is
/// ended early leaves a part that the next goes on from.
#[test]
fn what_tips_lead_to_is_found_from_what_earlier_tips_led_to_by_reading_what_is_new() {
    let work = alt_os::temp::tempdir().unwrap();
    let source = work.path().join("source");
    fs::create_dir(&source).unwrap();
    git(&source, &["init", "-q", "--initial-branch=main"]);
    let commit = |n: usize| {
        // twelve directories, of which a commit changes one
        let dir = source.join(format!("d{}", n % 12));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("file"), format!("revision {n}\n")).unwrap();
        fs::write(dir.join(format!("added-{n}")), format!("{n}\n")).unwrap();
        git(&source, &["add", "-A"]);
        git(&source, &["commit", "-q", "-m", &format!("revision {n}")]);
        text(git(&source, &["rev-parse", "HEAD"]))
    };
    let early: Vec<String> = (0..30).map(commit).collect();
    git(&source, &["tag", "-a", "v1", "-m", "tagged"]);
    let tag = text(git(&source, &["rev-parse", "v1"]));
    git(&source, &["checkout", "-q", "-b", "side", &early[20]]);
    let side = commit(100);
    git(&source, &["checkout", "-q", "main"]);
    let first = git_in(
        &source,
        &["pack-objects", "--stdout", "-q", "--revs"],
        b"main\nside\nv1\n",
    );
    let late: Vec<String> = (30..34).map(commit).collect();
    let second = git_in(
        &source,
        &["pack-objects", "--stdout", "-q", "--revs", "--thin"],
        format!("main\n^{}\n", early[29]).as_bytes(),
    );
    let one = SegmentSet::new(vec![segment(
        work.path(),
        "one.seg",
        &first,
        &SegmentSet::default(),
    )]);
    let two = segment(work.path(), "two.seg", &second, &one);
    let set = SegmentSet::new(vec![one.segments()[0].clone(), two]);
    let ids = |names: &[&String]| names.iter().map(|name| oid(name)).collect::<Vec<_>>();

    // from nothing
    let before = set
        .led_to(&ids(&[&early[29], &tag]), usize::MAX, None)
        .unwrap();
    same(&source, &before, &[&early[29], &tag]);
    assert!(!before.to(&oid(&side)) && !before.to(&oid(&late[0])));
    assert!(!set.all_led(&before).unwrap());

    // from what the earlier tips led to: five commits, each with the tree of
    // the whole and of the directory it changed, are ten trees and no more
    let now = [&late[3], &tag, &side];
    let none = set.led_beyond(before.clone(), &ids(&now), 9, None);
    assert!(matches!(none.1, Err(Error::Limit(_))), "nine trees");
    let (after, ended) = set.led_beyond(before.clone(), &ids(&now), 10, None);
    ended.unwrap();
    same(&source, &after, &[&late[3], &tag, &side]);
    assert!(set.all_led(&after).unwrap());
    // what it was found from is as it was
    same(&source, &before, &[&early[29], &tag]);
    // the same from nothing takes every tree there is
    let anew = set.led_to(&ids(&now), usize::MAX, None).unwrap();
    assert_eq!(anew.len(), after.len());
    assert!(matches!(
        set.led_to(&ids(&now), 60, None),
        Err(Error::Limit(_))
    ));

    // ended early, time after time: each part is led to by the tips, whole in
    // itself, and more than the one before, until it is all of it
    let mut part = Led::default();
    let mut walks = 0;
    loop {
        let had = part.len();
        let (next, ended) = set.led_beyond(part, &ids(&now), 7, None);
        part = next;
        walks += 1;
        let looked_at: &[&str] = if walks % 4 == 2 {
            &[&late[3], &tag, &side]
        } else {
            &[]
        };
        for id in objects(&source, looked_at) {
            if !part.to(&oid(&id)) {
                continue;
            }
            // with an object, what it refers to
            let below = objects(&source, &[&id]);
            assert!(below.iter().all(|id| part.to(&oid(id))), "{id}");
        }
        match ended {
            Ok(()) => break,
            Err(Error::Limit(_)) => assert!(part.len() > had, "walk {walks}"),
            Err(error) => panic!("{error}"),
        }
    }
    assert!(walks > 5, "{walks}");
    assert_eq!(part.len(), after.len());
    same(&source, &part, &[&late[3], &tag, &side]);

    // nobody waits: nothing is read
    let gone = std::sync::atomic::AtomicBool::new(true);
    let (left, ended) = set.led_beyond(before.clone(), &ids(&now), usize::MAX, Some(&gone));
    assert!(matches!(ended, Err(Error::GivenUp)));
    assert_eq!(left.len(), before.len());
    assert!(matches!(
        set.led_to(&[[7; 20]], usize::MAX, None),
        Err(Error::NotFound(_))
    ));

    // what tips lead to as the commits tell it: through a tag, below a tip,
    // and not beside it
    let lead = |tips: &[&String], sought: &[&String]| {
        set.lead_to_all(&ids(tips), &ids(sought), usize::MAX)
            .unwrap()
    };
    assert!(lead(&[&tag], &[&early[29], &early[0]]));
    assert!(lead(&[&late[3]], &[&early[29]]) && lead(&[&side], &[&early[20]]));
    assert!(!lead(&[&late[3]], &[&side]) && !lead(&[&side], &[&early[21]]));
    assert!(!lead(&[&late[3]], &[&early[29], &side]));
    // a tag is led to by no commit, nor is a tree
    assert!(!lead(&[&late[3]], &[&tag]));
    assert!(lead(&[&late[3]], &[]) && !lead(&[], &[&early[0]]));

    // several asked in turn share one walk's steps; nobody waits: nothing is gone through
    let mut steps = 40;
    let asked = |steps: &mut usize, gone| {
        set.lead_to_all_while(&ids(&[&late[3]]), &ids(&[&early[0]]), steps, gone)
    };
    assert!(asked(&mut steps, None).unwrap());
    assert_eq!(steps, 40 - 34);
    assert!(matches!(asked(&mut steps, None), Err(Error::Limit(_))));
    assert!(matches!(asked(&mut 40, Some(&gone)), Err(Error::GivenUp)));
}
