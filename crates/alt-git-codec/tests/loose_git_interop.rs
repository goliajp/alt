//! Reads every loose object of real git repositories (SHA-1 and SHA-256)
//! and verifies each one by re-hashing.

use std::fs;
use std::path::Path;
use std::process::Command;

use alt_git_codec::{HashAlgo, LooseStore, ObjectId};

fn git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "-c",
            "user.name=alt",
            "-c",
            "user.email=alt@test",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "tag.gpgsign=false",
        ])
        .args(args)
        .output()
        .expect("git must be runnable");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn make_repo(dir: &Path, object_format: &str) {
    let out = Command::new("git")
        .args(["init", "-q", "--object-format", object_format])
        .arg(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git init failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    fs::write(dir.join("a.txt"), "hello world\n").unwrap();
    fs::write(dir.join("b.bin"), [0u8, 159, 146, 150]).unwrap();
    fs::create_dir(dir.join("sub")).unwrap();
    fs::write(dir.join("sub/c.txt"), "nested\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", "first"]);
    git(dir, &["tag", "-a", "v0", "-m", "annotated tag"]);
    fs::write(dir.join("a.txt"), "hello again\n").unwrap();
    git(dir, &["commit", "-q", "-am", "second"]);
}

/// Walks `.git/objects/xx/…`, reads each object, re-hashes, returns the count.
fn verify_all_loose(repo: &Path, algo: HashAlgo) -> usize {
    let objects = repo.join(".git/objects");
    let store = LooseStore::new(&objects);
    let mut count = 0;
    for entry in fs::read_dir(&objects).unwrap() {
        let entry = entry.unwrap();
        let fanout = entry.file_name().into_string().unwrap();
        if fanout.len() != 2 {
            continue; // info/, pack/
        }
        for obj in fs::read_dir(entry.path()).unwrap() {
            let rest = obj.unwrap().file_name().into_string().unwrap();
            let oid = ObjectId::from_hex(format!("{fanout}{rest}").as_bytes()).unwrap();
            assert_eq!(oid.algo(), algo);
            let raw = store.read(&oid).unwrap();
            assert_eq!(
                ObjectId::hash_object(algo, raw.kind, &raw.data),
                oid,
                "re-hash mismatch for {oid}"
            );
            count += 1;
        }
    }
    count
}

#[test]
fn reads_all_loose_objects_of_a_sha1_repo() {
    let tmp = tempfile::tempdir().unwrap();
    make_repo(tmp.path(), "sha1");
    // 4 blobs + 3 trees + 2 commits + 1 tag
    let n = verify_all_loose(tmp.path(), HashAlgo::Sha1);
    assert!(n >= 10, "expected >= 10 loose objects, got {n}");
}

#[test]
fn reads_all_loose_objects_of_a_sha256_repo() {
    let tmp = tempfile::tempdir().unwrap();
    make_repo(tmp.path(), "sha256");
    let n = verify_all_loose(tmp.path(), HashAlgo::Sha256);
    assert!(n >= 10, "expected >= 10 loose objects, got {n}");
}
