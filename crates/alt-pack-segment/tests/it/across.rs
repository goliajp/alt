//! An object that several segments hold is read whichever of them holds it
//! how: a pack may bring an object the segments before it have, whole or as
//! a delta, and as a delta of an object that is itself a delta of it there.

use super::readable::{Of, pack};
use super::waiting::{id, object};
use super::{LIMITS, git, git_in, oid, text};
use alt_pack_segment::{Error, Segment, SegmentSet, ingest, merge};
use std::{collections::BTreeSet, fs, path::Path, sync::Arc};

fn noise(state: &mut u64) -> usize {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    (*state >> 16) as usize
}

fn written(dir: &Path, name: &str, bytes: &[u8]) -> Arc<Segment> {
    let path = dir.join(name);
    fs::write(&path, bytes).unwrap();
    Arc::new(Segment::open(&path).unwrap())
}

/// Every object a segment of `segments` tells of, read out of them together.
fn all_read(segments: &[Arc<Segment>], what: &str) -> BTreeSet<[u8; 20]> {
    let set = SegmentSet::new(segments.to_vec());
    let mut ids = BTreeSet::new();
    for segment in segments {
        for at in 0..segment.len() {
            ids.insert(segment.record(at).unwrap().oid);
        }
    }
    for oid in &ids {
        match set.read(oid) {
            Ok((_, data)) => assert_eq!(&id(&data), oid, "{what}"),
            Err(error) => panic!("{what}: an object is not read: {error}"),
        }
    }
    ids
}

/// Packs that follow each other into one repository, of a few objects between
/// them: each makes its objects of nothing, of entries of its own, and by id of
/// objects of its own or of the segments before it, and brings again what those
/// hold. After each that is taken in, and after segments that follow each other
/// are merged, every object of every segment is read.
#[test]
fn what_packs_bring_again_behind_one_another_is_read_from_every_segment() {
    let work = alt_os::temp::tempdir().unwrap();
    let mut state = 0x2545_f491_4f6c_dd1du64;
    let (mut taken, mut refused, mut again, mut merged, mut anew) = (0, 0, 0, 0, 0);
    for round in 0..300 {
        let universe = 3 + noise(&mut state) % 8;
        let mut segments: Vec<Arc<Segment>> = Vec::new();
        for push in 0..2 + noise(&mut state) % 5 {
            let count = 1 + noise(&mut state) % 6;
            let mut entries: Vec<(usize, Of)> = Vec::new();
            for at in 0..count {
                let n = noise(&mut state) % universe;
                let of = match noise(&mut state) % 8 {
                    0..=1 => Of::Nothing,
                    2..=3 if at > 0 => Of::Entry(noise(&mut state) % at),
                    _ if segments.is_empty() && at == 0 => Of::Nothing,
                    _ => Of::Object(noise(&mut state) % universe),
                };
                entries.push((n, of));
            }
            let pack = pack(&entries);
            let set = SegmentSet::new(segments.clone());
            let what = format!("round {round}, pack {push}");
            let ingested = match ingest(&pack, &set, &LIMITS) {
                Ok(ingested) => ingested,
                // made of what is nowhere, or of one another and of nothing else
                Err(Error::MissingBase(_) | Error::Format(_)) => {
                    refused += 1;
                    continue;
                }
                Err(error) => panic!("{what}: {error:?}"),
            };
            // the same pack behind the same segments is the same segment
            let mut index = Vec::new();
            ingested.write_index(&mut index).unwrap();
            for _ in 0..2 {
                let mut second = Vec::new();
                let anew = ingest(&pack, &set, &LIMITS).unwrap();
                anew.write_index(&mut second).unwrap();
                assert_eq!(index, second, "{what}");
            }
            // verified behind fewer of the segments and told anew behind all of
            // them, it is the segment it is when it is verified behind all of them
            for fewer in 0..segments.len() {
                let before = SegmentSet::new(segments[..fewer].to_vec());
                let Ok(mut moved) = ingest(&pack, &before, &LIMITS) else {
                    continue;
                };
                let mut first = Vec::new();
                moved.write_index(&mut first).unwrap();
                moved.behind(&set).unwrap();
                let mut second = Vec::new();
                moved.write_index(&mut second).unwrap();
                assert_eq!(index, second, "{what}, behind {fewer} at first");
                anew += usize::from(first != second);
            }
            taken += 1;
            again += usize::from(ingested.flags() & alt_pack_segment::FLAG_OVERLAP != 0);
            let mut bytes = pack;
            bytes.extend_from_slice(&index);
            segments.push(written(work.path(), &format!("{round}-{push}"), &bytes));
            let held = all_read(&segments, &what);
            if segments.len() > 1 && noise(&mut state).is_multiple_of(3) {
                let from = noise(&mut state) % (segments.len() - 1);
                let to = from + 2 + noise(&mut state) % (segments.len() - from - 1);
                let older = SegmentSet::new(segments[..from].to_vec());
                let what = format!("{what}, segments {from}..{to} merged");
                let bytes = merge(&segments[from..to], &older).expect(&what);
                let one = written(work.path(), &format!("{round}-{push}-merged"), &bytes);
                segments.splice(from..to, [one]);
                assert_eq!(all_read(&segments, &what), held, "{what}");
                merged += 1;
            }
        }
    }
    // all of these happen among the packs
    let counts = (taken, refused, again, merged, anew);
    assert!(
        taken > 300 && refused > 0 && again > 100 && merged > 50 && anew > 50,
        "{counts:?}"
    );
    println!("taken, refused, bringing again, merges, told anew: {counts:?}");
}

/// A delta that names an object the pack carries and an earlier segment holds
/// may be resolved on either, by whichever walk comes to it first. The index
/// tells one of them, the same each time.
#[test]
fn a_delta_of_what_is_in_its_pack_and_before_it_has_one_index() {
    let work = alt_os::temp::tempdir().unwrap();
    let none = SegmentSet::default();
    let first = pack(&[(0, Of::Nothing), (1, Of::Nothing), (2, Of::Entry(1))]);
    let before = SegmentSet::new(vec![super::segment(work.path(), "first", &first, &none)]);
    // the first object again, of one made of it before; then many of each
    let mut entries = vec![(3, Of::Object(0)), (0, Of::Entry(0)), (2, Of::Object(1))];
    for n in 4..300 {
        entries.push((n, Of::Object(n % 3)));
    }
    let pack = pack(&entries);
    let index = |_| {
        let mut index = Vec::new();
        let ingested = ingest(&pack, &before, &LIMITS).unwrap();
        ingested.write_index(&mut index).unwrap();
        index
    };
    let indexes: BTreeSet<Vec<u8>> = (0..60).map(index).collect();
    assert_eq!(indexes.len(), 1);
    let mut bytes = pack.clone();
    bytes.extend_from_slice(indexes.first().unwrap());
    let second = written(work.path(), "second", &bytes);
    let both = [before.segments()[0].clone(), second];
    assert_eq!(all_read(&both, "both").len(), 300);
    for n in 0..300 {
        let data = object(n);
        let set = SegmentSet::new(both.to_vec());
        assert_eq!(set.read(&id(&data)).unwrap().1, data, "object {n}");
    }
}

/// A file is changed and the change reverted, each commit pushed by itself as
/// git packs a push: the second pack has the new content as a delta of the
/// old, the third the old as a delta of the new. Every object is read, a
/// clone of the last commit is a pack git takes, and so it is after merges.
#[test]
fn what_a_revert_brings_back_is_read_and_sent() {
    let work = alt_os::temp::tempdir().unwrap();
    let source = work.path().join("source");
    fs::create_dir(&source).unwrap();
    git(&source, &["init", "-q", "--initial-branch=main"]);
    let body = |changed: bool| -> String {
        let line = |n: usize| match changed && n % 50 == 7 {
            true => format!("line {n} as it was changed\n"),
            false => format!("line {n} of the file as it was first written\n"),
        };
        (0..600).map(line).collect()
    };
    let mut commits: Vec<String> = Vec::new();
    for (changed, message) in [(false, "first"), (true, "changed")] {
        fs::write(source.join("file.txt"), body(changed)).unwrap();
        git(&source, &["add", "-A"]);
        git(&source, &["commit", "-q", "-m", message]);
        commits.push(text(git(&source, &["rev-parse", "HEAD"])));
    }
    git(&source, &["revert", "--no-edit", "HEAD"]);
    commits.push(text(git(&source, &["rev-parse", "HEAD"])));
    let mut segments: Vec<Arc<Segment>> = Vec::new();
    for (at, commit) in commits.iter().enumerate() {
        let revs = match at {
            0 => format!("{commit}\n"),
            _ => format!("{commit}\n^{}\n", commits[at - 1]),
        };
        let args = ["pack-objects", "--thin", "--stdout", "-q", "--revs"];
        let pack = git_in(&source, &args, revs.as_bytes());
        let set = SegmentSet::new(segments.clone());
        segments.push(super::segment(
            work.path(),
            &format!("{at}.seg"),
            &pack,
            &set,
        ));
    }
    // the shape this is about: the old content is in the last pack, as a delta
    let old = oid(&text(git(
        &source,
        &["rev-parse", &format!("{}:file.txt", commits[0])],
    )));
    assert!(segments[2].find(&old).is_some() && segments[0].find(&old).is_some());
    let everything = super::objects(&source, &[&commits[2]]);
    let merges = [
        ("as pushed", None),
        ("the last two merged", Some(1..3)),
        ("the first two merged", Some(0..2)),
        ("all merged", Some(0..3)),
    ];
    for (what, parts) in merges {
        let mut segments = segments.clone();
        if let Some(parts) = parts {
            let older = SegmentSet::new(segments[..parts.start].to_vec());
            let bytes = merge(&segments[parts.clone()], &older).expect(what);
            let one = written(work.path(), what, &bytes);
            segments.splice(parts, [one]);
        }
        let set = SegmentSet::new(segments);
        super::check_objects(&source, &set, &everything);
        let selection = set.select(&[oid(&commits[2])], &[]).unwrap();
        assert_eq!(selection.len(), everything.len(), "{what}");
        let mut sent = Vec::new();
        set.write_selected(&selection, false, &mut sent).unwrap();
        let target = work.path().join(format!("target {what}"));
        fs::create_dir(&target).unwrap();
        git(&target, &["init", "-q", "--bare"]);
        git_in(&target, &["index-pack", "--stdin", "--strict"], &sent);
        git(&target, &["update-ref", "refs/heads/main", &commits[2]]);
        git(&target, &["fsck", "--strict", "--full"]);
    }
}
