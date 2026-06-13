//! L1 round-trip fidelity over the real corpus: every repo under
//! `$ALT_CORPUS` goes import → export, and the exported `.git` must
//!
//!   ① re-hash every object to its declared id, holding exactly the
//!      store's object set (no object dropped, none invented);
//!   ② answer with the same refs and HEAD as the source;
//!   ③ pass `git fsck` (held to the same bar the source meets — see ③
//!      below for why not `--strict`);
//!   ④ match the source git's own view — full history (`log`) and the
//!      complete object inventory (`cat-file --batch-all-objects`).
//!
//! This is the resident automation that closes VISION §8.1 at the export
//! boundary; the fixture-scale version lives in `export_cycle.rs`.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use alt_export::export_git;
use alt_git_codec::{HashAlgo, ObjectId};
use alt_git_pack::IndexedPack;
use alt_odb::NativeOdb;
use alt_repo::Repository;

fn git(repo: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .current_dir(repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args(args)
        .output()
        .unwrap()
}

/// Asserts source and exported git agree on both the success flag and the
/// stdout bytes — handles detached HEAD (where `symbolic-ref` fails on
/// both) without a special case.
fn git_eq(source: &Path, target: &Path, args: &[&str]) {
    let a = git(source, args);
    let b = git(target, args);
    assert_eq!(
        a.status.success(),
        b.status.success(),
        "git {args:?} success differs (source {:?})",
        String::from_utf8_lossy(&a.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&a.stdout),
        String::from_utf8_lossy(&b.stdout),
        "git {args:?} stdout differs between source and export"
    );
}

/// Every object id git can see in `repo`, type and size included, sorted —
/// git's own inventory, comparable across repositories.
fn git_inventory(repo: &Path) -> String {
    let out = git(
        repo,
        &[
            "cat-file",
            "--batch-check",
            "--batch-all-objects",
            "--unordered",
        ],
    );
    assert!(out.status.success(), "cat-file in {repo:?} failed");
    let mut lines: Vec<&str> = std::str::from_utf8(&out.stdout).unwrap().lines().collect();
    lines.sort_unstable();
    lines.join("\n")
}

/// The set of object ids physically present in a source repo (loose +
/// every pack), so the export can be checked to preserve it exactly.
fn source_oids(repo_dir: &Path, algo: HashAlgo) -> BTreeSet<ObjectId> {
    let mut oids = BTreeSet::new();
    let pack_dir = repo_dir.join(".git/objects/pack");
    if let Ok(entries) = fs::read_dir(&pack_dir) {
        for entry in entries {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "pack") {
                let indexed = IndexedPack::open(&path, algo).unwrap();
                let idx = indexed.idx();
                for i in 0..idx.len() {
                    oids.insert(idx.oid_at(i));
                }
            }
        }
    }
    alt_testutil::for_each_loose(repo_dir, |oid, _| {
        oids.insert(oid);
    });
    oids
}

/// The single plain pack the export wrote: every entry re-hashed to its
/// declared id, returning the id set.
fn exported_oids(target: &Path, algo: HashAlgo) -> BTreeSet<ObjectId> {
    let pack_dir = target.join(".git/objects/pack");
    let pack = fs::read_dir(&pack_dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|e| e == "pack"))
        .expect("export wrote a pack");
    let indexed = IndexedPack::open(&pack, algo).unwrap();
    let idx = indexed.idx();
    let mut oids = BTreeSet::new();
    let mut order: Vec<(u64, u32)> = (0..idx.len())
        .map(|i| (idx.offset_at(i).unwrap(), i))
        .collect();
    order.sort_unstable();
    for (offset, i) in order {
        let oid = idx.oid_at(i);
        let obj = indexed.read_at(offset).unwrap();
        assert_eq!(
            ObjectId::hash_object(algo, obj.kind, &obj.data),
            oid,
            "exported object {oid} re-hash mismatch in {target:?}"
        );
        oids.insert(oid);
    }
    oids
}

#[test]
#[ignore = "needs $ALT_CORPUS pointing at a directory of git repos"]
fn corpus_roundtrip_l1_fidelity() {
    let corpus = std::env::var("ALT_CORPUS").expect("set ALT_CORPUS to the corpus directory");
    let mut repos = 0;
    for entry in fs::read_dir(&corpus).unwrap() {
        let repo_dir = entry.unwrap().path();
        if !repo_dir.join(".git").is_dir() {
            continue;
        }
        repos += 1;
        let repo = Repository::discover(&repo_dir).unwrap();
        let algo = repo.algo();

        let alt_root = tempfile::tempdir().unwrap();
        let alt_dir = alt_root.path().join(".alt");
        alt_import::import_git(&repo, &alt_dir, "test/roundtrip", 1).unwrap();

        let out_root = tempfile::tempdir().unwrap();
        let target = out_root.path().join("exported");
        let report = export_git(&alt_dir, &target).unwrap();
        assert!(report.head, "{repo_dir:?}: export wrote no HEAD");
        // refs may legitimately be zero: an unborn HEAD (gitflow-loose
        // points HEAD at a branch that has no commit yet) carries no refs
        // at all. The git_eq checks below validate ref equality directly.

        // ① object set: re-hash every exported object and require the
        // exported set to equal the store's set and the source's set
        let store_len = NativeOdb::open(&alt_dir).unwrap().len();
        let exported = exported_oids(&target, algo);
        assert_eq!(
            exported.len(),
            report.objects as usize,
            "{repo_dir:?}: exported pack count vs report",
        );
        assert_eq!(
            exported.len(),
            store_len,
            "{repo_dir:?}: exported pack count vs store",
        );
        let source = source_oids(&repo_dir, algo);
        assert_eq!(
            source, exported,
            "{repo_dir:?}: exported object set differs from source",
        );

        // ③ git is the referee. Plain `git fsck`, not `--strict`: the
        // store holds the source's bytes verbatim, so the export is
        // exactly as git-valid as the source — no more. Real corpora
        // (gitflow-mirror) carry legacy trees with zero-padded filemodes
        // that even the source fails `--strict` on; holding the export to
        // a stricter bar than the source would contradict L1 fidelity.
        // Plain fsck still catches every real corruption (missing/broken/
        // malformed objects, bad sha, broken connectivity).
        let fsck = git(&target, &["fsck"]);
        assert!(
            fsck.status.success(),
            "{repo_dir:?}: git fsck: {}{}",
            String::from_utf8_lossy(&fsck.stdout),
            String::from_utf8_lossy(&fsck.stderr),
        );

        // ② refs + HEAD, ④ full history and the whole object inventory,
        // source vs exported through git's own eyes
        git_eq(&repo_dir, &target, &["for-each-ref"]);
        git_eq(&repo_dir, &target, &["symbolic-ref", "HEAD"]);
        git_eq(&repo_dir, &target, &["rev-parse", "HEAD"]);
        git_eq(&repo_dir, &target, &["log", "--pretty=raw", "--all"]);
        assert_eq!(
            git_inventory(&repo_dir),
            git_inventory(&target),
            "{repo_dir:?}: object inventory differs",
        );

        eprintln!(
            "{repo_dir:?}: {} objects round-tripped ({} refs, {algo:?})",
            exported.len(),
            report.refs,
        );
    }
    assert!(repos > 0, "no repos found under $ALT_CORPUS");
}
