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

/// A pack of the one entry `entry` of kind 7: a delta of the object `base`
/// that copies all `len` bytes of it.
fn copied(base: &[u8; 20], len: usize) -> Vec<u8> {
    use super::waiting::{header, zlib};
    let mut pack = b"PACK".to_vec();
    pack.extend_from_slice(&2u32.to_be_bytes());
    pack.extend_from_slice(&1u32.to_be_bytes());
    let script = [len as u8, len as u8, 0x90, len as u8];
    pack.extend(header(7, script.len()));
    pack.extend_from_slice(base);
    pack.extend(zlib(&script));
    let trailer = alt_hash::sha1(&pack);
    pack.extend_from_slice(&trailer);
    pack
}

/// The segments keep the commit of a branch that was deleted, its tree and its
/// file. One who is led to the other branches alone sends packs that refer to
/// them: as a parent, as the entry of a tree, as the base of a delta. Each is
/// refused, and taken where the sender is led to everything; what refers to
/// the branches that are there is taken from either.
#[test]
fn a_pack_refers_to_what_its_sender_is_led_to_and_to_nothing_else_the_segments_hold() {
    use alt_pack_segment::{Limits, ingest, ingest_led};
    let limits = Limits::of(1 << 20, 1 << 20, 1 << 30);
    let work = alt_os::temp::tempdir().unwrap();
    let source = work.path().join("source");
    fs::create_dir(&source).unwrap();
    git(&source, &["init", "-q", "--initial-branch=main"]);
    let commit = |file: &str, content: &str| {
        fs::write(source.join(file), content).unwrap();
        git(&source, &["add", "-A"]);
        git(&source, &["commit", "-q", "-m", file]);
        text(git(&source, &["rev-parse", "HEAD"]))
    };
    let main = commit("kept", "kept\n");
    git(&source, &["checkout", "-q", "-b", "gone"]);
    let gone = commit("secret", "what the deleted branch held\n");
    let secret = text(git(&source, &["rev-parse", "gone:secret"]));
    git(&source, &["checkout", "-q", "main"]);
    let pack = git_in(
        &source,
        &["pack-objects", "--stdout", "-q", "--revs"],
        b"main\ngone\n",
    );
    let none = SegmentSet::default();
    let set = SegmentSet::new(vec![segment(work.path(), "all.seg", &pack, &none)]);
    let led = set.led_to(&[oid(&main)], usize::MAX, None).unwrap();
    assert!(led.to(&oid(&main)) && !led.to(&oid(&gone)) && !led.to(&oid(&secret)));
    assert_eq!(led.len(), 3);
    let everything = set
        .led_to(&[oid(&main), oid(&gone)], usize::MAX, None)
        .unwrap();
    assert_eq!(everything.len() as u64, set.objects());
    // a pack of the objects named and of no others
    let sent = |named: &[&str]| {
        let ids: String = (named.iter())
            .map(|name| text(git(&source, &["rev-parse", name])) + "\n")
            .collect();
        git_in(&source, &["pack-objects", "--stdout", "-q"], ids.as_bytes())
    };
    let of = |commit: &str, rest: &str| format!("{commit}{rest}");
    let refused = |pack: &[u8], what: &str| {
        let taken = ingest_led(pack, (&set, &led), &limits, None);
        assert!(
            matches!(taken, Err(Error::Dangling(..) | Error::MissingBase(_))),
            "{what}: {:?}",
            taken.map(|taken| taken.objects())
        );
        ingest(pack, &set, &limits).unwrap_or_else(|error| panic!("{what}: {error}"));
        ingest_led(pack, (&set, &everything), &limits, None)
            .unwrap_or_else(|error| panic!("{what}: {error}"));
    };

    // a commit whose parent is the commit of the deleted branch
    git(&source, &["checkout", "-q", "--detach", &gone]);
    let child = commit("child", "child\n");
    let pack = sent(&[&child, &of(&child, "^{tree}"), &of(&child, ":child")]);
    refused(&pack, "the parent");
    // a commit of the branch that is there whose tree names the file of the
    // deleted branch, which the pack does not carry
    git(&source, &["checkout", "-q", "main"]);
    let named = commit("copy", "what the deleted branch held\n");
    refused(&sent(&[&named, &of(&named, "^{tree}")]), "the file");
    // the file itself, as a delta that copies all of the one the segments hold
    refused(&copied(&oid(&secret), 29), "the base");
    // and carried whole, it is the sender's own: the commit, its tree, the file
    let pack = sent(&[&named, &of(&named, "^{tree}"), &of(&named, ":copy")]);
    let taken = ingest_led(&pack, (&set, &led), &limits, None).unwrap();
    assert_eq!(taken.objects(), 3);
}
