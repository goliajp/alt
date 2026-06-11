//! Compares RefStore against `git for-each-ref` / `git rev-parse` across
//! loose, packed, and mixed states.

use std::path::Path;

use alt_git_codec::HashAlgo;
use alt_git_refs::{RefStore, RefTarget};
use alt_testutil as common;

/// `git for-each-ref` ground truth: sorted `(resolved oid, name, peeled)`.
fn git_refs(repo: &Path) -> Vec<(String, String, String)> {
    common::git(
        repo,
        &[
            "for-each-ref",
            "--format=%(objectname) %(refname) %(*objectname)",
        ],
    )
    .lines()
    .map(|l| {
        let mut parts = l.splitn(3, ' ');
        (
            parts.next().unwrap().to_owned(),
            parts.next().unwrap().to_owned(),
            parts.next().unwrap_or("").to_owned(),
        )
    })
    .collect()
}

fn assert_matches_git(repo: &Path, algo: HashAlgo, expect_packed_peel: bool) {
    let store = RefStore::open(repo.join(".git"), algo).unwrap();
    let refs = store.iter_refs().unwrap();
    let ours: Vec<(String, String)> = refs
        .iter()
        .map(|r| {
            let resolved = match &r.target {
                RefTarget::Direct(oid) => oid.to_string(),
                RefTarget::Symbolic(name) => store
                    .resolve(&name.to_string())
                    .unwrap()
                    .expect("symref target must resolve")
                    .to_string(),
            };
            (resolved, r.name.to_string())
        })
        .collect();
    let git_truth = git_refs(repo);
    let git_names: Vec<(String, String)> = git_truth
        .iter()
        .map(|(oid, name, _)| (oid.clone(), name.clone()))
        .collect();
    assert_eq!(ours, git_names, "ref listing must match for-each-ref");

    // `%(*objectname)` peels the tag *object*; our `peeled` is storage-level
    // (packed-refs lines). The two oracles only coincide once the tag is
    // packed, because git writes packed-refs fully-peeled.
    if expect_packed_peel {
        let tag = refs.iter().find(|r| r.name == "refs/tags/v0").unwrap();
        let git_peel = &git_truth
            .iter()
            .find(|(_, name, _)| name == "refs/tags/v0")
            .unwrap()
            .2;
        assert_eq!(
            tag.peeled
                .expect("packed tag must carry a peeled oid")
                .to_string(),
            *git_peel,
            "packed-refs peeled line must match object-level peel"
        );
    }

    // HEAD must be a symref to main and resolve like rev-parse
    let head = store.read("HEAD").unwrap().unwrap();
    assert_eq!(
        head,
        RefTarget::Symbolic("refs/heads/main".into()),
        "fixture HEAD is on main"
    );
    assert_eq!(
        store.resolve("HEAD").unwrap().unwrap().to_string(),
        common::git(repo, &["rev-parse", "HEAD"]).trim()
    );
}

fn refs_match_in_all_states(algo: HashAlgo, object_format: &str) {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    common::make_repo(repo, object_format);

    // 1. loose state (fresh repo: no packed-refs)
    assert_matches_git(repo, algo, false);

    // 2. fully packed
    common::git(repo, &["pack-refs", "--all"]);
    assert_matches_git(repo, algo, true);

    // 3. mixed: loose updates shadowing packed entries, plus a new branch
    common::git(repo, &["branch", "-f", "feat", "HEAD~1"]);
    common::git(repo, &["branch", "fresh"]);
    assert_matches_git(repo, algo, true);
}

#[test]
fn refs_match_git_sha1() {
    refs_match_in_all_states(HashAlgo::Sha1, "sha1");
}

#[test]
fn refs_match_git_sha256() {
    refs_match_in_all_states(HashAlgo::Sha256, "sha256");
}
