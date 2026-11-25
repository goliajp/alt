//! The objects a walk from tips comes to, against `git rev-list --objects`.

use std::collections::{BTreeMap, BTreeSet};

use alt_git_codec::{ObjectId, ObjectKind};
use alt_repo::{Ends, Repository};
use alt_testutil as common;

/// The walk that names what it comes to lists what git lists, each object
/// once, and the two versions of a file under one number. A file is not read
/// for it: one that is gone from the store is listed all the same, where the
/// walk that reads every object leaves it out.
#[test]
fn what_tips_reach_is_listed_with_its_name_and_no_file_is_read_for_it() {
    let tmp = alt_os::temp::tempdir().unwrap();
    common::make_repo(tmp.path(), "sha1");
    let id = |spec: &str| -> ObjectId {
        let told = common::git(tmp.path(), &["rev-parse", spec]);
        told.trim().parse().unwrap()
    };
    let (main, first) = (id("main"), id("main~2"));
    let listed = |range: &str| -> BTreeSet<String> {
        let told = common::git(tmp.path(), &["rev-list", "--objects", range]);
        let ids = told.lines().map(|line| line[..40].to_owned());
        ids.collect()
    };
    let none = Ends::default();

    let repo = Repository::discover(tmp.path()).unwrap();
    for (excludes, range) in [
        (vec![], "main".to_owned()),
        (vec![first], "main~2..main".to_owned()),
    ] {
        let reached = repo
            .reachable_named_between(&[main], &excludes, &none)
            .unwrap();
        let ids: BTreeSet<String> = reached.iter().map(|at| at.oid.to_string()).collect();
        assert_eq!(ids.len(), reached.len(), "{range}");
        assert_eq!(ids, listed(&range), "{range}");
        let read = repo
            .reachable_objects_between(&[main], &excludes, &none)
            .unwrap();
        let kinds = |of: Vec<(ObjectId, ObjectKind)>| of.into_iter().collect::<BTreeMap<_, _>>();
        assert_eq!(
            kinds(reached.iter().map(|at| (at.oid, at.kind)).collect()),
            kinds(read),
            "{range}"
        );
    }

    let reached = repo.reachable_named_between(&[main], &[], &none).unwrap();
    let name = |oid: ObjectId| reached.iter().find(|at| at.oid == oid).unwrap().name;
    let (old, new) = (id("main~2:big.txt"), id("main:big.txt"));
    assert_ne!(old, new);
    assert_eq!(name(old), name(new));
    assert_ne!(name(new), name(id("main:a.txt")));
    assert_eq!(name(main), 0);
    assert_eq!(name(id("main^{tree}")), 0);
    assert_ne!(name(id("main:sub")), 0);

    let hex = new.to_string();
    let file = tmp
        .path()
        .join(".git/objects")
        .join(&hex[..2])
        .join(&hex[2..]);
    std::fs::remove_file(file).unwrap();
    let repo = Repository::discover(tmp.path()).unwrap();
    let reached = repo.reachable_named_between(&[main], &[], &none).unwrap();
    let there = reached.iter().find(|at| at.oid == new).unwrap();
    assert_eq!(there.kind, ObjectKind::Blob);
    let read = repo.reachable_objects(&[main], &[]).unwrap();
    assert!(read.iter().all(|at| at.0 != new));
    assert_eq!(read.len() + 1, reached.len());
}
