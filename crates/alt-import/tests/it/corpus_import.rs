//! Corpus import: each repo under `$ALT_CORPUS` migrates fully into its
//! own `.alt`; every object re-reads byte-identical and every ref matches
//! the git-side view. Plus kill -9 injection on the import itself: an
//! import killed at an arbitrary point must leave an openable store and a
//! re-run must converge to exactly the same end state.

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use alt_git_pack::IndexedPack;
use alt_import::import_git;
use alt_odb::NativeOdb;
use alt_refs::{RefStore, RefTarget};
use alt_repo::Repository;

fn verify_repo_against_store(repo_dir: &Path, alt_dir: &Path) -> u64 {
    let repo = Repository::discover(repo_dir).unwrap();
    let odb = NativeOdb::open(alt_dir).unwrap();
    let mut checked = 0u64;

    // packed objects
    let pack_dir = repo_dir.join(".git/objects/pack");
    if let Ok(entries) = fs::read_dir(&pack_dir) {
        for entry in entries {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "pack") {
                let indexed = IndexedPack::open(&path, repo.algo()).unwrap();
                let idx = indexed.idx();
                let mut order: Vec<(u64, u32)> = (0..idx.len())
                    .map(|i| (idx.offset_at(i).unwrap(), i))
                    .collect();
                order.sort_unstable();
                for (offset, i) in order {
                    let want = indexed.read_at(offset).unwrap();
                    let oid = idx.oid_at(i);
                    let got = odb.get(&oid).unwrap().unwrap();
                    assert_eq!(got.kind, want.kind, "{oid}");
                    assert_eq!(got.data, *want.data, "{oid}");
                    checked += 1;
                }
            }
        }
    }
    // loose objects
    checked += alt_testutil::for_each_loose(repo_dir, |oid, raw| {
        let got = odb.get(&oid).unwrap().unwrap();
        assert_eq!(got.kind, raw.kind, "{oid}");
        assert_eq!(got.data, raw.data, "{oid}");
    }) as u64;

    // refs incl. HEAD
    let native = RefStore::open(alt_dir).unwrap();
    for r in repo.git_refs().unwrap().iter_refs().unwrap() {
        let name = std::str::from_utf8(&r.name).unwrap();
        let got = native.get(name).unwrap_or_else(|| panic!("missing {name}"));
        match (&r.target, got) {
            (alt_git_refs::RefTarget::Direct(want), RefTarget::Oid(oid)) => {
                assert_eq!(want, oid, "{name}")
            }
            (alt_git_refs::RefTarget::Symbolic(want), RefTarget::Symbolic(sym)) => {
                assert_eq!(std::str::from_utf8(want).unwrap(), sym, "{name}")
            }
            (want, got) => panic!("{name}: {want:?} vs {got:?}"),
        }
    }
    checked
}

#[test]
#[ignore = "needs $ALT_CORPUS pointing at a directory of git repos"]
fn corpus_repos_import_fully_and_idempotently() {
    let corpus = std::env::var("ALT_CORPUS").expect("set ALT_CORPUS to the corpus directory");
    for entry in fs::read_dir(&corpus).unwrap() {
        let repo_dir = entry.unwrap().path();
        if !repo_dir.join(".git").is_dir() {
            continue;
        }
        let alt_root = tempfile::tempdir().unwrap();
        let alt_dir = alt_root.path().join(".alt");

        let repo = Repository::discover(&repo_dir).unwrap();
        let report = import_git(&repo, &alt_dir, "test/corpus", 1).unwrap();
        assert!(report.op.is_some());

        let rerun = import_git(&repo, &alt_dir, "test/corpus", 2).unwrap();
        assert_eq!(rerun.objects_new, 0, "{repo_dir:?} rerun must be no-op");
        assert!(rerun.op.is_none(), "{repo_dir:?} rerun must record no op");

        let checked = verify_repo_against_store(&repo_dir, &alt_dir);
        eprintln!(
            "{repo_dir:?}: {} objects imported, {checked} read back verified, {} refs, \
             {} lineage deltas",
            report.objects_seen, report.refs_seen, report.lineage_deltas
        );
    }
}

/// Imports `$ALT_CRASH_SRC` into `$ALT_CRASH_DIR`. Helper for the kill -9
/// test below; a no-op without the env vars (it also runs in plain
/// `--ignored` sweeps, where it must pass instantly).
#[test]
#[ignore = "helper child workload, spawned by killed_import_recovers_and_converges"]
fn import_child_workload() {
    let (Ok(src), Ok(dst)) = (
        std::env::var("ALT_CRASH_SRC"),
        std::env::var("ALT_CRASH_DIR"),
    ) else {
        return;
    };
    let repo = Repository::discover(Path::new(&src)).unwrap();
    import_git(&repo, Path::new(&dst), "test/crash-import", 1).unwrap();
}

#[test]
#[ignore = "needs $ALT_CORPUS pointing at a directory of git repos"]
fn killed_import_recovers_and_converges() {
    let corpus = std::env::var("ALT_CORPUS").expect("set ALT_CORPUS to the corpus directory");
    // pick the biggest standard repo available for a wide kill window
    let src = ["libgit2", "cargo", "git", "gitflow-loose"]
        .iter()
        .map(|name| Path::new(&corpus).join(name))
        .find(|p| p.join(".git").is_dir())
        .expect("corpus has at least one repo");

    let alt_root = tempfile::tempdir().unwrap();
    let alt_dir = alt_root.path().join(".alt");
    let deadline = Duration::from_secs(60);

    // kill the import mid-objects (first pack visibly growing)
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "corpus_import::import_child_workload",
        ])
        .env("ALT_CRASH_SRC", &src)
        .env("ALT_CRASH_DIR", &alt_dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pack = alt_dir.join("store/pack-00000001.altpack");
    let start = Instant::now();
    loop {
        let size = fs::metadata(&pack).map(|m| m.len()).unwrap_or(0);
        if size > 4 << 20 {
            break;
        }
        assert!(
            start.elapsed() < deadline,
            "import made no progress (pack at {size} bytes)"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    child.kill().unwrap();
    child.wait().unwrap();

    // the partial store must open cleanly and hold no refs (the ref op
    // comes after all objects: an interrupted import never half-publishes)
    {
        let native = RefStore::open(&alt_dir).unwrap();
        assert!(
            native.is_empty(),
            "a killed import must not have published refs"
        );
        let odb = NativeOdb::open(&alt_dir).unwrap();
        // whatever was mapped must read back (spot-check a sample)
        let sample: Vec<_> = odb.entries().take(64).cloned().collect();
        for entry in &sample {
            let raw = odb.get(&entry.git).unwrap().unwrap();
            assert_eq!(raw.kind, entry.kind);
            assert_eq!(raw.data.len() as u64, entry.size);
        }
        assert!(!sample.is_empty() || odb.is_empty());
    }

    // re-run to completion and verify full convergence
    let repo = Repository::discover(&src).unwrap();
    let report = import_git(&repo, &alt_dir, "test/crash-import", 2).unwrap();
    assert!(report.op.is_some(), "completion publishes the refs");
    let checked = verify_repo_against_store(&src, &alt_dir);
    eprintln!(
        "killed-import convergence: {} objects seen, {checked} verified after recovery",
        report.objects_seen
    );
}
