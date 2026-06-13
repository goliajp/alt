//! The M2 numbers (CP3 exit evidence): per corpus repo — .alt volume vs
//! git's object store, import throughput, and full-store read throughput
//! through the .alt backend vs the M1 direct-.git path.
//!
//! Guarded by ALT_BENCH=1 on top of --ignored: timing belongs to release
//! builds run by hand, not to the (debug) corpus gate. No ratio
//! assertions — numbers are reported, the plan doc records them.

use std::fs;
use std::path::Path;
use std::time::Instant;

use alt_import::import_git;
use alt_odb::NativeOdb;
use alt_repo::Repository;

/// Recursive byte size of a directory.
fn dir_bytes(dir: &Path) -> u64 {
    let mut total = 0;
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries {
            let entry = entry.unwrap();
            let meta = entry.metadata().unwrap();
            if meta.is_dir() {
                total += dir_bytes(&entry.path());
            } else {
                total += meta.len();
            }
        }
    }
    total
}

#[test]
#[ignore = "bench: set ALT_BENCH=1 and $ALT_CORPUS, run by hand in release"]
fn corpus_bench_volume_and_throughput() {
    if std::env::var("ALT_BENCH").as_deref() != Ok("1") {
        eprintln!("corpus_bench: skipped (set ALT_BENCH=1 to run)");
        return;
    }
    let corpus = std::env::var("ALT_CORPUS").expect("set ALT_CORPUS to the corpus directory");

    eprintln!(
        "| repo | git objects | .alt raw | .alt compact | ratio | import | read .alt | read .git (M1) |"
    );
    eprintln!("|---|---|---|---|---|---|---|---|");

    for entry in fs::read_dir(&corpus).unwrap() {
        let repo_dir = entry.unwrap().path();
        if !repo_dir.join(".git").is_dir() {
            continue;
        }
        let name = repo_dir.file_name().unwrap().to_string_lossy().into_owned();
        let git_bytes = dir_bytes(&repo_dir.join(".git/objects"));

        // --- import throughput (fresh store) ---
        let alt_root = tempfile::tempdir().unwrap();
        let alt_dir = alt_root.path().join(".alt");
        let repo = Repository::discover(&repo_dir).unwrap();
        let t0 = Instant::now();
        let report = import_git(&repo, &alt_dir, "bench", 1).unwrap();
        let import_s = t0.elapsed().as_secs_f64();
        let alt_raw = dir_bytes(&alt_dir);

        // --- compact: reclaim the dead weight from lineage delta re-encoding
        // (S5/S6 supersede full tree/commit/blob records; S7 drops them) ---
        let compact = {
            let mut odb = NativeOdb::open(&alt_dir).unwrap();
            let r = odb.compact().unwrap();
            odb.flush().unwrap();
            r
        };
        let alt_bytes = dir_bytes(&alt_dir);

        // --- full-store read throughput, .alt backend (post-compaction) ---
        let odb = NativeOdb::open(&alt_dir).unwrap();
        let oids: Vec<_> = odb.entries().map(|e| e.git).collect();
        let t0 = Instant::now();
        let mut alt_read_bytes = 0u64;
        for oid in &oids {
            alt_read_bytes += odb.get(oid).unwrap().unwrap().data.len() as u64;
        }
        let alt_read_s = t0.elapsed().as_secs_f64();

        // --- same reads through the M1 direct-.git path ---
        let t0 = Instant::now();
        let mut git_read_bytes = 0u64;
        for oid in &oids {
            git_read_bytes += repo.read_object(oid).unwrap().unwrap().data.len() as u64;
        }
        let git_read_s = t0.elapsed().as_secs_f64();
        assert_eq!(
            alt_read_bytes, git_read_bytes,
            "both paths read the same bytes"
        );

        let mib = |b: u64| b as f64 / (1 << 20) as f64;
        eprintln!(
            "| {name} | {:.1} MiB | {:.1} MiB | {:.1} MiB | {:.2}x | {:.1}s ({:.0} obj/s) | {:.1}s ({:.0} MiB/s) | {:.1}s ({:.0} MiB/s) |",
            mib(git_bytes),
            mib(alt_raw),
            mib(alt_bytes),
            alt_bytes as f64 / git_bytes as f64,
            import_s,
            report.objects_seen as f64 / import_s,
            alt_read_s,
            mib(alt_read_bytes) / alt_read_s,
            git_read_s,
            mib(git_read_bytes) / git_read_s,
        );
        eprintln!(
            "  ↳ lineage deltas: {} total ({} tree, {} commit); compaction {} -> {} packs, reclaimed {:.1} MiB",
            report.lineage_deltas,
            report.tree_lineage_deltas,
            report.commit_lineage_deltas,
            compact.packs_before,
            compact.packs_after,
            mib(compact.bytes_before.saturating_sub(compact.bytes_after)),
        );
    }
}
