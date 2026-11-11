//! segments merged into one

use super::{check_objects, git, git_in, objects, repository, segment, text};
use alt_pack_segment::{Segment, SegmentSet, merge};
use std::{fs, path::Path, sync::Arc};

/// one segment per commit of `source`, each a thin pack on top of the ones before
fn pushed_one_by_one(work: &Path, source: &Path) -> Vec<Arc<Segment>> {
    let commits = text(git(source, &["rev-list", "--reverse", "main"]));
    let mut segments: Vec<Arc<Segment>> = Vec::new();
    let mut previous: Option<&str> = None;
    for (index, commit) in commits.lines().enumerate() {
        let range = match previous {
            Some(previous) => format!("{commit}\n^{previous}\n"),
            None => format!("{commit}\n"),
        };
        let pack = git_in(
            source,
            &[
                "pack-objects",
                "--thin",
                "--stdout",
                "-q",
                "--revs",
                "--delta-base-offset",
            ],
            range.as_bytes(),
        );
        let existing = SegmentSet::new(segments.clone());
        segments.push(segment(
            work,
            &format!("push-{index}.seg"),
            &pack,
            &existing,
        ));
        previous = Some(commit);
    }
    segments
}

fn stored(work: &Path, name: &str, bytes: &[u8]) -> Arc<Segment> {
    let path = work.join(name);
    fs::write(&path, bytes).unwrap();
    Arc::new(Segment::open(&path).unwrap())
}

/// the whole set resent as one pack is a repository Git accepts
fn clones(work: &Path, source: &Path, set: &SegmentSet, name: &str) {
    assert!(set.verbatim());
    let mut sent = Vec::new();
    set.write_all(&mut sent).unwrap();
    let target = work.join(name);
    fs::create_dir(&target).unwrap();
    git(&target, &["init", "-q", "--bare"]);
    git_in(&target, &["index-pack", "--stdin", "--strict"], &sent);
    let head = text(git(source, &["rev-parse", "main"]));
    git(&target, &["update-ref", "refs/heads/main", &head]);
    git(&target, &["fsck", "--strict", "--full"]);
}

#[test]
fn merged_segments_hold_every_object_once_with_short_chains() {
    let work = alt_os::temp::tempdir().unwrap();
    let source = work.path().join("source");
    fs::create_dir(&source).unwrap();
    // every commit rewrites the same file, so each push is a delta on the one before
    repository(&source, 70);
    let segments = pushed_one_by_one(work.path(), &source);
    assert_eq!(segments.len(), 70);
    let everything = objects(&source, &["main"]);

    let all = merge(&segments, &SegmentSet::default()).unwrap();
    let all = SegmentSet::new(vec![stored(work.path(), "all.seg", &all)]);
    assert_eq!(all.objects() as usize, everything.len());
    check_objects(&source, &all, &everything);
    clones(work.path(), &source, &all, "from-all");
    let only = &all.segments()[0];
    let depths: Vec<u16> = (0..only.len())
        .map(|index| only.record(index).unwrap().depth)
        .collect();
    // the chain through seventy pushes is cut where it would pass fifty links
    assert_eq!(depths.iter().copied().max(), Some(50));

    // merging the newest pushes leaves deltas that rest on the older segments
    let (older, newer) = segments.split_at(20);
    let older = SegmentSet::new(older.to_vec());
    let tail = merge(newer, &older).unwrap();
    let mut both = older.segments().to_vec();
    both.push(stored(work.path(), "tail.seg", &tail));
    let both = SegmentSet::new(both);
    assert_eq!(both.objects() as usize, everything.len());
    check_objects(&source, &both, &everything);
    clones(work.path(), &source, &both, "from-both");
    // a chain that continues in an older segment counts those links too
    let last = both.segments().last().unwrap();
    assert!((0..last.len()).all(|index| last.record(index).unwrap().depth <= 50));

    // a merge of the merge and what came later: objects carried twice are kept once
    let again = git_in(
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
    let repeated = segment(work.path(), "again.seg", &again, &all);
    assert_ne!(repeated.flags(), 0);
    let merged = merge(
        &[all.segments()[0].clone(), repeated],
        &SegmentSet::default(),
    )
    .unwrap();
    let merged = SegmentSet::new(vec![stored(work.path(), "merged.seg", &merged)]);
    // the repeated pack also brought the tag
    let with_tag = objects(&source, &["--all"]);
    assert_eq!(merged.objects() as usize, with_tag.len());
    check_objects(&source, &merged, &with_tag);
    clones(work.path(), &source, &merged, "from-merged");
}
