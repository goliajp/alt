//! Import semantics on real (fixture) repositories: full migration,
//! idempotence, convergence after upstream movement, and re-entrancy from
//! a partial state.

use std::path::Path;

use alt_git_codec::ObjectId;
use alt_import::import_git;
use alt_odb::NativeOdb;
use alt_refs::{RefStore, RefTarget};
use alt_repo::Repository;

fn import_here(repo_dir: &Path, alt_dir: &Path) -> alt_import::ImportReport {
    let repo = Repository::discover(repo_dir).unwrap();
    import_git(&repo, alt_dir, "test/import", 1000).unwrap()
}

/// Every loose object of the source must be readable from the .alt store
/// with identical kind and bytes.
fn assert_objects_migrated(repo_dir: &Path, alt_dir: &Path) -> usize {
    let odb = NativeOdb::open(alt_dir).unwrap();
    alt_testutil::for_each_loose(repo_dir, |oid, raw| {
        let back = odb.get(&oid).unwrap().unwrap();
        assert_eq!(back.kind, raw.kind, "kind mismatch for {oid}");
        assert_eq!(back.data, raw.data, "bytes mismatch for {oid}");
    })
}

#[test]
fn full_import_migrates_objects_refs_head_and_config() {
    let repo_dir = tempfile::tempdir().unwrap();
    alt_testutil::make_repo(repo_dir.path(), "sha1");
    let alt_root = tempfile::tempdir().unwrap();
    let alt_dir = alt_root.path().join(".alt");

    let report = import_here(repo_dir.path(), &alt_dir);
    assert!(report.objects_seen > 0);
    assert_eq!(report.objects_new, report.objects_seen, "fresh store");
    assert!(report.op.is_some(), "first import records exactly one op");

    let migrated = assert_objects_migrated(repo_dir.path(), &alt_dir);
    assert!(migrated > 0);

    // refs: compare against the git-side view, name by name
    let repo = Repository::discover(repo_dir.path()).unwrap();
    let native = RefStore::open(&alt_dir).unwrap();
    let git_refs = repo.git_refs().unwrap().iter_refs().unwrap();
    assert_eq!(native.len(), git_refs.len() + 1, "all refs + HEAD");
    for r in &git_refs {
        let name = std::str::from_utf8(&r.name).unwrap();
        match (&r.target, native.get(name).unwrap()) {
            (alt_git_refs::RefTarget::Direct(want), RefTarget::Oid(got)) => {
                assert_eq!(want, got, "{name}")
            }
            (alt_git_refs::RefTarget::Symbolic(want), RefTarget::Symbolic(got)) => {
                assert_eq!(std::str::from_utf8(want).unwrap(), got, "{name}")
            }
            (want, got) => panic!("{name}: target shape mismatch: {want:?} vs {got:?}"),
        }
    }
    // HEAD is the symref git left it as
    assert!(matches!(
        native.get("HEAD").unwrap(),
        RefTarget::Symbolic(t) if t == "refs/heads/main"
    ));
    // resolving HEAD through the native store equals git's resolution
    let git_head = repo.git_refs().unwrap().resolve("HEAD").unwrap().unwrap();
    assert_eq!(native.resolve("HEAD").unwrap(), Some(git_head));

    // contract 2: config preserved byte-for-byte
    let src = std::fs::read(repo_dir.path().join(".git/config")).unwrap();
    let kept = std::fs::read(alt_dir.join("git-import/config")).unwrap();
    assert_eq!(src, kept);

    // exactly one op in the log
    assert_eq!(native.oplog().len(), 1);
}

#[test]
fn reimport_of_unchanged_source_is_a_no_op() {
    let repo_dir = tempfile::tempdir().unwrap();
    alt_testutil::make_repo(repo_dir.path(), "sha1");
    let alt_root = tempfile::tempdir().unwrap();
    let alt_dir = alt_root.path().join(".alt");

    let first = import_here(repo_dir.path(), &alt_dir);
    let second = import_here(repo_dir.path(), &alt_dir);
    assert_eq!(second.objects_new, 0);
    assert_eq!(second.refs_changed, 0);
    assert!(second.op.is_none(), "a converged rerun records no op");
    assert_eq!(second.objects_seen, first.objects_seen);

    let native = RefStore::open(&alt_dir).unwrap();
    assert_eq!(native.oplog().len(), 1, "still exactly one op");
}

#[test]
fn reimport_after_upstream_moves_updates_only_what_moved() {
    let repo_dir = tempfile::tempdir().unwrap();
    alt_testutil::make_repo(repo_dir.path(), "sha1");
    let alt_root = tempfile::tempdir().unwrap();
    let alt_dir = alt_root.path().join(".alt");
    import_here(repo_dir.path(), &alt_dir);

    // advance main upstream
    std::fs::write(repo_dir.path().join("new-file"), "fresh content\n").unwrap();
    alt_testutil::git(repo_dir.path(), &["add", "new-file"]);
    alt_testutil::git(repo_dir.path(), &["commit", "-q", "-m", "advance"]);

    let report = import_here(repo_dir.path(), &alt_dir);
    assert!(report.objects_new > 0, "the new commit's objects");
    assert!(report.refs_changed >= 1, "main moved");
    assert!(report.op.is_some());

    let repo = Repository::discover(repo_dir.path()).unwrap();
    let native = RefStore::open(&alt_dir).unwrap();
    let want: ObjectId = repo
        .git_refs()
        .unwrap()
        .resolve("refs/heads/main")
        .unwrap()
        .unwrap();
    assert_eq!(native.resolve("refs/heads/main").unwrap(), Some(want));
    assert_eq!(native.oplog().len(), 2, "one more op, not a rewrite");
    assert_objects_migrated(repo_dir.path(), &alt_dir);
}

#[test]
fn import_completes_from_a_partial_object_state() {
    let repo_dir = tempfile::tempdir().unwrap();
    alt_testutil::make_repo(repo_dir.path(), "sha1");
    let alt_root = tempfile::tempdir().unwrap();
    let alt_dir = alt_root.path().join(".alt");

    // simulate an import that died after migrating some objects but
    // before the ref op: pre-put a few objects, no refs
    {
        let mut odb = NativeOdb::open(&alt_dir).unwrap();
        let mut first3 = 0;
        alt_testutil::for_each_loose(repo_dir.path(), |oid, raw| {
            if first3 < 3 {
                odb.put(oid, raw.kind, &raw.data).unwrap();
                first3 += 1;
            }
        });
        odb.flush().unwrap();
    }

    let report = import_here(repo_dir.path(), &alt_dir);
    assert_eq!(
        report.objects_new + 3,
        report.objects_seen,
        "completion writes exactly the missing objects"
    );
    assert!(report.op.is_some(), "refs were still missing");
    assert_objects_migrated(repo_dir.path(), &alt_dir);
}

#[test]
fn import_delta_encodes_same_path_history() {
    let repo_dir = tempfile::tempdir().unwrap();
    alt_testutil::make_repo(repo_dir.path(), "sha1");

    // a file evolving across commits: each version shares most content
    let file = repo_dir.path().join("evolving.txt");
    let body = "the quick brown fox jumps over the lazy dog\n".repeat(50);
    for round in 0..4 {
        std::fs::write(&file, format!("{body}version {round}\n")).unwrap();
        alt_testutil::git(repo_dir.path(), &["add", "evolving.txt"]);
        alt_testutil::git(
            repo_dir.path(),
            &["commit", "-q", "-m", &format!("evolve {round}")],
        );
    }

    let alt_root = tempfile::tempdir().unwrap();
    let alt_dir = alt_root.path().join(".alt");
    let report = import_here(repo_dir.path(), &alt_dir);
    assert!(
        report.lineage_deltas >= 3,
        "three predecessors of evolving.txt must delta, got {}",
        report.lineage_deltas
    );
    // each of those commits also changed the root tree, so the predecessor
    // trees delta too (M3.5 S5 — the main volume win)
    assert!(
        report.tree_lineage_deltas >= 3,
        "predecessor trees must delta, got {}",
        report.tree_lineage_deltas
    );
    // and the parent commits delta against their children (M3.5 S6)
    assert!(
        report.commit_lineage_deltas >= 3,
        "parent commits must delta, got {}",
        report.commit_lineage_deltas
    );

    // every object still reads back byte-identical through the chains —
    // trees included, now that they are delta-encoded
    assert_objects_migrated(repo_dir.path(), &alt_dir);

    // idempotent rerun: nothing new to re-encode, no op
    let rerun = import_here(repo_dir.path(), &alt_dir);
    assert_eq!(rerun.lineage_deltas, 0);
    assert!(rerun.op.is_none());
}
