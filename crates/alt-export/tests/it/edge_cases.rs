//! Edge and bad-path round trips for export. The fixture-scale happy
//! path (merges, annotated tag, symlink, gitlink, exec bit, awkward
//! names) lives in `export_cycle.rs`; here we cover the corners that a
//! normal `git commit` workflow never produces:
//!
//!   - an empty repository with an unborn HEAD (zero objects, zero refs);
//!   - the empty tree object;
//!   - a chain of annotated tags, plus a dangling orphan tag object;
//!   - a non-UTF-8 path name in a tree;
//!   - symlink / gitlink / exec mode bits asserted explicitly.
//!
//! Each fixture goes import → export and is checked through git's own
//! eyes. Plus: export refuses a non-empty target loudly (also covered in
//! export_cycle.rs; kept here next to the other bad paths).

use std::path::Path;
use std::process::{Command, Output, Stdio};

use alt_export::export_git;
use alt_repo::Repository;

/// git with a fixed identity and gpg disabled, an optional stdin, and no
/// success assertion — callers that build objects need the stdout, callers
/// probing behaviour need the status.
fn run_git(repo: &Path, args: &[&str], stdin: Option<&[u8]>) -> Output {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
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
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if stdin.is_some() {
        cmd.stdin(Stdio::piped());
    }
    let mut child = cmd.spawn().unwrap();
    if let Some(bytes) = stdin {
        use std::io::Write;
        child.stdin.take().unwrap().write_all(bytes).unwrap();
    }
    child.wait_with_output().unwrap()
}

fn git_ok(repo: &Path, args: &[&str], stdin: Option<&[u8]>) -> Vec<u8> {
    let out = run_git(repo, args, stdin);
    assert!(
        out.status.success(),
        "git {args:?} in {repo:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

fn trimmed(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes).unwrap().trim().to_owned()
}

fn init(dir: &Path) {
    let out = run_git(dir, &["init", "-q", "-b", "main"], None);
    assert!(out.status.success());
}

/// import → export, then hold the export to source's view: same refs and
/// HEAD, same history, same full object inventory, and a clean `git fsck`
/// (plain, not `--strict` — see corpus_roundtrip.rs ③). Returns the
/// exported `.git` root for any fixture-specific extra assertions.
fn round_trip(source: &Path) -> tempfile::TempDir {
    let alt_root = tempfile::tempdir().unwrap();
    let alt_dir = alt_root.path().join(".alt");
    let repo = Repository::discover(source).unwrap();
    alt_import::import_git(&repo, &alt_dir, "test/edge", 1).unwrap();

    let out_root = tempfile::tempdir().unwrap();
    let target = out_root.path().join("exported");
    let report = export_git(&alt_dir, &target).unwrap();
    assert!(report.head, "{source:?}: export wrote no HEAD");

    let fsck = run_git(&target, &["fsck"], None);
    assert!(
        fsck.status.success(),
        "{source:?}: git fsck: {}{}",
        String::from_utf8_lossy(&fsck.stdout),
        String::from_utf8_lossy(&fsck.stderr),
    );

    for args in [
        &["for-each-ref"][..],
        &["symbolic-ref", "HEAD"],
        &["rev-parse", "HEAD"],
        &["log", "--pretty=raw", "--all"],
    ] {
        let a = run_git(source, args, None);
        let b = run_git(&target, args, None);
        assert_eq!(
            a.status.success(),
            b.status.success(),
            "{source:?}: git {args:?} success differs",
        );
        assert_eq!(
            String::from_utf8_lossy(&a.stdout),
            String::from_utf8_lossy(&b.stdout),
            "{source:?}: git {args:?} differs",
        );
    }

    // full object inventory through git: id + type + size, sorted
    let inv = |repo: &Path| -> String {
        let mut lines: Vec<String> = String::from_utf8(git_ok(
            repo,
            &[
                "cat-file",
                "--batch-check",
                "--batch-all-objects",
                "--unordered",
            ],
            None,
        ))
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
        lines.sort();
        lines.join("\n")
    };
    assert_eq!(inv(source), inv(&target), "{source:?}: inventory differs");

    // hand the live TempDir back so the export survives for the caller's
    // fixture-specific assertions (target = out_root/exported)
    out_root
}

/// Writes `content` as a blob and returns its 40/64-hex id.
fn blob(repo: &Path, content: &[u8]) -> String {
    trimmed(git_ok(
        repo,
        &["hash-object", "-w", "--stdin"],
        Some(content),
    ))
}

#[test]
fn empty_repo_with_unborn_head() {
    let source = tempfile::tempdir().unwrap();
    init(source.path());
    // unborn HEAD: a symbolic HEAD pointing at a branch with no commit,
    // zero objects, zero refs
    let alt_root = tempfile::tempdir().unwrap();
    let alt_dir = alt_root.path().join(".alt");
    let repo = Repository::discover(source.path()).unwrap();
    alt_import::import_git(&repo, &alt_dir, "test/edge", 1).unwrap();

    let out_root = tempfile::tempdir().unwrap();
    let target = out_root.path().join("exported");
    let report = export_git(&alt_dir, &target).unwrap();
    assert_eq!(report.objects, 0, "empty repo exports no objects");
    assert_eq!(report.refs, 0, "empty repo exports no refs");
    assert!(report.head, "unborn HEAD is still written");

    assert!(run_git(&target, &["fsck"], None).status.success());
    assert_eq!(
        trimmed(git_ok(&target, &["symbolic-ref", "HEAD"], None)),
        "refs/heads/main",
    );
    assert!(
        run_git(&target, &["for-each-ref"], None).stdout.is_empty(),
        "an unborn repo has no refs",
    );
}

#[test]
fn empty_tree_object() {
    let source = tempfile::tempdir().unwrap();
    init(source.path());
    // the empty-tree commit: its tree is git's well-known empty tree
    git_ok(
        source.path(),
        &["commit", "-q", "--allow-empty", "-m", "empty"],
        None,
    );
    let tree = trimmed(git_ok(source.path(), &["rev-parse", "HEAD^{tree}"], None));

    let out_root = round_trip(source.path());
    let target = out_root.path().join("exported");
    assert_eq!(
        trimmed(git_ok(&target, &["cat-file", "-t", &tree], None)),
        "tree",
        "empty tree object must be present in the export",
    );
    assert!(
        run_git(&target, &["cat-file", "-p", &tree], None)
            .stdout
            .is_empty(),
        "the empty tree has no entries",
    );
}

#[test]
fn nested_annotated_tag_chain_and_orphan() {
    let source = tempfile::tempdir().unwrap();
    init(source.path());
    git_ok(
        source.path(),
        &["commit", "-q", "--allow-empty", "-m", "base"],
        None,
    );

    // v1 -> commit, v2 -> v1, v3 -> v2: an annotated tag pointing at an
    // annotated tag pointing at an annotated tag
    git_ok(source.path(), &["tag", "-a", "v1", "-m", "tag one"], None);
    let v1 = trimmed(git_ok(source.path(), &["rev-parse", "v1"], None));
    git_ok(
        source.path(),
        &["tag", "-a", "v2", &v1, "-m", "tag two"],
        None,
    );
    let v2 = trimmed(git_ok(source.path(), &["rev-parse", "v2"], None));
    git_ok(
        source.path(),
        &["tag", "-a", "v3", &v2, "-m", "tag three"],
        None,
    );

    // a dangling orphan tag: the object exists loose but no ref names it
    git_ok(
        source.path(),
        &["tag", "-a", "orphan", "-m", "to be detached"],
        None,
    );
    let orphan = trimmed(git_ok(source.path(), &["rev-parse", "orphan"], None));
    git_ok(source.path(), &["tag", "-d", "orphan"], None);

    let out_root = round_trip(source.path());
    let target = out_root.path().join("exported");
    // the whole tag chain resolves on the export side
    assert_eq!(
        trimmed(git_ok(&target, &["rev-parse", "v3^{commit}"], None)),
        trimmed(git_ok(source.path(), &["rev-parse", "v3^{commit}"], None)),
    );
    // the orphan tag object survived the round trip (inventory equality in
    // round_trip already enforces it, but assert the type explicitly)
    assert_eq!(
        trimmed(git_ok(&target, &["cat-file", "-t", &orphan], None)),
        "tag",
    );
}

#[test]
fn non_utf8_path_name() {
    let source = tempfile::tempdir().unwrap();
    init(source.path());
    let oid = blob(source.path(), b"bytes\n");

    // a tree entry whose name is not valid UTF-8 (latin-1 'é' = 0xe9),
    // built with mktree so the working filesystem's name rules never
    // apply — git stores raw bytes in trees regardless
    let mut spec = Vec::new();
    spec.extend_from_slice(b"100644 blob ");
    spec.extend_from_slice(oid.as_bytes());
    spec.extend_from_slice(b"\tcaf\xe9.txt\n");
    let tree = trimmed(git_ok(source.path(), &["mktree"], Some(&spec)));
    let commit = trimmed(git_ok(
        source.path(),
        &["commit-tree", &tree, "-m", "non-utf8 path"],
        None,
    ));
    git_ok(
        source.path(),
        &["update-ref", "refs/heads/main", &commit],
        None,
    );

    let out_root = round_trip(source.path());
    let target = out_root.path().join("exported");
    // the raw name bytes survive byte-for-byte: that `git ls-tree <tree>`
    // resolves on the export side proves the tree object with that exact
    // oid (a function of the raw name bytes) was reconstructed, and the
    // listing matches the source's verbatim
    let want = git_ok(source.path(), &["ls-tree", &tree], None);
    let got = git_ok(&target, &["ls-tree", &tree], None);
    assert_eq!(want, got, "non-utf8 tree entry name must round-trip");
}

#[test]
fn symlink_gitlink_exec_modes() {
    let source = tempfile::tempdir().unwrap();
    init(source.path());
    let regular = blob(source.path(), b"plain\n");
    let exec = blob(source.path(), b"#!/bin/sh\n");
    let link = blob(source.path(), b"regular.txt"); // symlink target text
    // gitlink target: a real, present commit (an empty-tree commit), so
    // mktree's type check is satisfied — git never follows into it
    let empty_tree = trimmed(git_ok(
        source.path(),
        &["hash-object", "-t", "tree", "-w", "--stdin"],
        Some(b""),
    ));
    let gitlink = trimmed(git_ok(
        source.path(),
        &["commit-tree", &empty_tree, "-m", "sub"],
        None,
    ));

    let mut spec = Vec::new();
    for (mode, ty, oid, name) in [
        ("100644", "blob", regular.as_str(), "regular.txt"),
        ("100755", "blob", exec.as_str(), "run.sh"),
        ("120000", "blob", link.as_str(), "link"),
        ("160000", "commit", gitlink.as_str(), "submod"),
    ] {
        spec.extend_from_slice(format!("{mode} {ty} ").as_bytes());
        spec.extend_from_slice(oid.as_bytes());
        spec.extend_from_slice(format!("\t{name}\n").as_bytes());
    }
    let tree = trimmed(git_ok(source.path(), &["mktree"], Some(&spec)));
    let commit = trimmed(git_ok(
        source.path(),
        &["commit-tree", &tree, "-m", "modes"],
        None,
    ));
    git_ok(
        source.path(),
        &["update-ref", "refs/heads/main", &commit],
        None,
    );

    let out_root = round_trip(source.path());
    let target = out_root.path().join("exported");
    // every mode bit survives verbatim
    let listing = git_ok(&target, &["ls-tree", &tree], None);
    let listing = String::from_utf8(listing).unwrap();
    for mode in ["100644", "100755", "120000", "160000"] {
        assert!(
            listing.contains(mode),
            "mode {mode} missing from exported tree:\n{listing}",
        );
    }
}
