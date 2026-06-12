//! The .alt read backend: discovery, and read-API equivalence against the
//! same repository opened through .git.

use std::path::Path;

use alt_repo::Repository;

/// Builds a fixture repo and imports it into `<dir>/.alt`.
fn imported(repo_dir: &Path, alt_root: &Path) {
    let repo = Repository::discover(repo_dir).unwrap();
    alt_import::import_git(&repo, &alt_root.join(".alt"), "test/native", 1).unwrap();
}

#[test]
fn discovery_finds_and_prefers_alt() {
    let repo_dir = tempfile::tempdir().unwrap();
    alt_testutil::make_repo(repo_dir.path(), "sha1");
    let alt_root = tempfile::tempdir().unwrap();
    imported(repo_dir.path(), alt_root.path());

    // a nested working directory discovers upward to .alt
    let nested = alt_root.path().join("some/deep/dir");
    std::fs::create_dir_all(&nested).unwrap();
    let repo = Repository::discover(&nested).unwrap();
    assert!(repo.is_native());
    assert_eq!(repo.git_dir().file_name().unwrap(), ".alt");
}

#[test]
fn read_api_is_equivalent_across_backends() {
    let repo_dir = tempfile::tempdir().unwrap();
    alt_testutil::make_repo(repo_dir.path(), "sha1");
    let alt_root = tempfile::tempdir().unwrap();
    imported(repo_dir.path(), alt_root.path());

    let git = Repository::discover(repo_dir.path()).unwrap();
    let alt = Repository::discover(alt_root.path()).unwrap();
    assert!(!git.is_native());
    assert!(alt.is_native());
    assert_eq!(git.algo(), alt.algo());

    // rev-parse equivalence over the DWIM space
    for spec in ["HEAD", "main", "feat", "v0", "refs/heads/main"] {
        assert_eq!(
            git.rev_parse(spec).unwrap(),
            alt.rev_parse(spec).unwrap(),
            "{spec}"
        );
    }

    // every loose object reads identically through the alt backend
    let count = alt_testutil::for_each_loose(repo_dir.path(), |oid, raw| {
        let got = alt.read_object(&oid).unwrap().unwrap();
        assert_eq!(got.kind, raw.kind, "{oid}");
        assert_eq!(got.data, raw.data, "{oid}");
    });
    assert!(count > 0);

    // revwalk equivalence from HEAD
    let head = git.rev_parse("HEAD").unwrap().unwrap();
    let git_walk: Vec<_> = git
        .rev_walk(head)
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let alt_walk: Vec<_> = alt
        .rev_walk(head)
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(git_walk, alt_walk);
}
