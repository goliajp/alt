//! Parses every loose commit/tree/tag of real git repositories and checks
//! that serialization reproduces the original bytes exactly (L1 fidelity,
//! read direction).

use alt_git_codec::{Commit, HashAlgo, ObjectKind, Tag, Tree};

use alt_testutil as common;

fn roundtrip_repo(algo: HashAlgo, object_format: &str) {
    let tmp = tempfile::tempdir().unwrap();
    common::make_repo(tmp.path(), object_format);

    let (mut commits, mut trees, mut tags) = (0, 0, 0);
    let mut merge_commits = 0;
    let mut modes_seen: Vec<String> = Vec::new();

    common::for_each_loose(tmp.path(), |oid, raw| {
        let reserialized = match raw.kind {
            ObjectKind::Blob => return,
            ObjectKind::Commit => {
                commits += 1;
                let commit = Commit::parse(&raw.data).unwrap();
                assert_eq!(commit.tree().unwrap().algo(), algo);
                if commit.parents().count() == 2 {
                    merge_commits += 1;
                }
                commit.serialize()
            }
            ObjectKind::Tree => {
                trees += 1;
                let tree = Tree::parse(&raw.data, algo).unwrap();
                for entry in &tree.entries {
                    let mode = entry.mode.as_str().to_owned();
                    if !modes_seen.contains(&mode) {
                        modes_seen.push(mode);
                    }
                }
                tree.serialize()
            }
            ObjectKind::Tag => {
                tags += 1;
                let tag = Tag::parse(&raw.data).unwrap();
                assert_eq!(tag.target_kind(), Some(ObjectKind::Commit));
                tag.serialize()
            }
        };
        assert_eq!(reserialized, raw.data, "round-trip mismatch for {oid}");
    });

    assert!(commits >= 5, "expected >= 5 commits, got {commits}");
    assert!(trees >= 6, "expected >= 6 trees, got {trees}");
    assert_eq!(tags, 1);
    assert_eq!(merge_commits, 1);
    for mode in ["100644", "100755", "120000", "160000", "40000"] {
        assert!(
            modes_seen.iter().any(|m| m == mode),
            "fixture should exercise mode {mode}, saw {modes_seen:?}"
        );
    }
}

#[test]
fn round_trips_all_objects_of_a_sha1_repo() {
    roundtrip_repo(HashAlgo::Sha1, "sha1");
}

#[test]
fn round_trips_all_objects_of_a_sha256_repo() {
    roundtrip_repo(HashAlgo::Sha256, "sha256");
}
