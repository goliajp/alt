//! A name that leads out of the git directory names no ref, whatever
//! file lies where it leads.

use alt_git_codec::HashAlgo;
use alt_git_refs::RefStore;
use alt_testutil as common;

#[test]
fn a_name_with_a_step_out_of_the_directory_reads_as_no_ref() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    common::make_repo(&repo, "sha1");
    let store = RefStore::open(repo.join(".git"), HashAlgo::Sha1).unwrap();
    let head = store.resolve("HEAD").unwrap().expect("a commit");
    // a file beside the repository that reads as a ref
    std::fs::write(tmp.path().join("outside"), format!("{head}\n")).unwrap();
    std::fs::write(repo.join("in-tree"), format!("{head}\n")).unwrap();
    for name in [
        "../../outside",
        "../in-tree",
        "refs/heads/../../../in-tree",
        "refs//heads/main",
        "./HEAD",
        "/etc/hostname",
    ] {
        assert!(store.read(name).unwrap().is_none(), "{name}");
    }
    assert!(store.read("HEAD").unwrap().is_some());
}
