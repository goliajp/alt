//! Repository facade vs git: discovery, rev-parse, and walk order
//! (`git log --format=%H` is the ground truth).

use std::path::Path;

use alt_git_codec::HashAlgo;
use alt_repo::Repository;
use alt_testutil as common;

fn assert_repo_matches(repo_path: &Path, algo: HashAlgo) {
    let repo = Repository::discover(repo_path).unwrap();
    assert_eq!(repo.algo(), algo);

    // rev-parse over the DWIM forms
    for spec in ["HEAD", "main", "feat", "v0", "refs/heads/main"] {
        let ours = repo.rev_parse(spec).unwrap();
        let truth = common::git(repo_path, &["rev-parse", &format!("{spec}^{{}}")]);
        let (peeled, _) = repo.peel(ours.expect("spec must resolve")).unwrap();
        assert_eq!(peeled.to_string(), truth.trim(), "rev-parse {spec}");
    }

    // full-history walk order
    let head = repo.rev_parse("HEAD").unwrap().unwrap();
    let ours: Vec<String> = repo
        .rev_walk(head)
        .unwrap()
        .map(|r| r.unwrap().0.to_string())
        .collect();
    let truth: Vec<String> = common::git(repo_path, &["log", "--format=%H"])
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(ours, truth, "walk order must match git log");
}

#[test]
fn repository_matches_git() {
    for (algo, object_format, ref_format) in [
        (HashAlgo::Sha1, "sha1", "files"),
        (HashAlgo::Sha256, "sha256", "files"),
        (HashAlgo::Sha1, "sha1", "reftable"),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        common::make_repo_opts(tmp.path(), object_format, ref_format);
        // loose state, then fully packed state
        assert_repo_matches(tmp.path(), algo);
        common::pack_repo(tmp.path());
        assert_repo_matches(tmp.path(), algo);
    }
}

#[test]
fn discovers_from_subdirectory_and_dot_git_file() {
    let tmp = tempfile::tempdir().unwrap();
    common::make_repo(tmp.path(), "sha1");
    let repo = Repository::discover(&tmp.path().join("sub")).unwrap();
    assert_eq!(
        repo.work_tree().unwrap(),
        tmp.path().canonicalize().unwrap()
    );
}
