//! the index of a pack does not depend on how much of it was kept in memory

use super::{LIMITS, git, git_in, repository, segment, text};
use alt_pack_segment::{SegmentSet, ingest, ingest_holding};
use std::fs;

fn index(pack: &[u8], existing: &SegmentSet, held: u64) -> Vec<u8> {
    let mut out = Vec::new();
    ingest_holding(pack, existing, &LIMITS, held)
        .unwrap()
        .write_index(&mut out)
        .unwrap();
    out
}

#[test]
fn a_pack_verifies_the_same_whatever_stays_in_memory() {
    let work = alt_os::temp::tempdir().unwrap();
    let source = work.path().join("source");
    fs::create_dir(&source).unwrap();
    repository(&source, 12);
    let commits = text(git(&source, &["rev-list", "--reverse", "main"]));
    let commits: Vec<&str> = commits.lines().collect();
    // a pack with deltas inside it, and a thin one whose deltas rest on the first
    let first = git_in(
        &source,
        &[
            "pack-objects",
            "--stdout",
            "-q",
            "--revs",
            "--delta-base-offset",
        ],
        format!("{}\n", commits[7]).as_bytes(),
    );
    let thin = git_in(
        &source,
        &[
            "pack-objects",
            "--thin",
            "--stdout",
            "-q",
            "--revs",
            "--delta-base-offset",
        ],
        format!("{}\n^{}\n", commits[11], commits[7]).as_bytes(),
    );
    let none = SegmentSet::default();
    let base = SegmentSet::new(vec![segment(work.path(), "first.seg", &first, &none)]);

    let mut expected = Vec::new();
    ingest(&first, &none, &LIMITS)
        .unwrap()
        .write_index(&mut expected)
        .unwrap();
    // nothing kept, a little kept, everything kept
    for held in [0, 700, 20_000, u64::MAX] {
        assert_eq!(index(&first, &none, held), expected, "holding {held} bytes");
    }
    let expected = index(&thin, &base, u64::MAX);
    for held in [0, 300, 5_000] {
        assert_eq!(index(&thin, &base, held), expected, "holding {held} bytes");
    }

    // a damaged object is refused in the pass that happens to meet it
    let mut damaged = first.clone();
    let middle = damaged.len() / 2;
    damaged[middle] ^= 0x10;
    for held in [0, u64::MAX] {
        assert!(ingest_holding(&damaged, &none, &LIMITS, held).is_err());
    }
}
