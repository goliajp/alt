//! M7-B2 large-files bench: import volume + throughput on the binary-asset
//! fixture against git's own object store, then a "modify and re-import"
//! pass to expose the incremental story (CDC chunk dedup + lineage delta
//! on growing blobs). Numbers are reported, the plan doc records them —
//! no ratio assertion here; the judgment lives in the plan's CP-7B note.
//!
//! Gated on `ALT_BENCH=1` plus `--ignored`, like the source-corpus bench,
//! so timing belongs to release builds run by hand, not the debug gate.
//! Defaults to `.dev/corpus/large-files`; override with
//! `ALT_LARGE_FILES_CORPUS=<dir>` (the `scripts/build-large-corpus.sh`
//! builder writes to the default path).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use alt_import::import_git;
use alt_odb::NativeOdb;
use alt_repo::Repository;

fn dir_bytes(dir: &Path) -> u64 {
    let mut total = 0u64;
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
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

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "alt-bench")
        .env("GIT_AUTHOR_EMAIL", "alt-bench@test")
        .env("GIT_COMMITTER_NAME", "alt-bench")
        .env("GIT_COMMITTER_EMAIL", "alt-bench@test")
        .env("GIT_AUTHOR_DATE", "1700000001 +0000")
        .env("GIT_COMMITTER_DATE", "1700000001 +0000")
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
#[ignore = "bench: set ALT_BENCH=1 (and optionally ALT_LARGE_FILES_CORPUS), run by hand in release"]
fn large_files_bench_volume_and_throughput() {
    if std::env::var("ALT_BENCH").as_deref() != Ok("1") {
        eprintln!("large_files_bench: skipped (set ALT_BENCH=1 to run)");
        return;
    }
    let corpus = std::env::var("ALT_LARGE_FILES_CORPUS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(".dev/corpus/large-files"));
    if !corpus.join(".git").is_dir() {
        panic!(
            "{} does not look like a git repo — run scripts/build-large-corpus.sh first",
            corpus.display()
        );
    }

    let git_bytes_before = dir_bytes(&corpus.join(".git/objects"));
    let working_bytes = dir_bytes(&corpus) - dir_bytes(&corpus.join(".git"));

    let repo = Repository::discover(&corpus).unwrap();
    let alt_root = tempfile::tempdir().unwrap();
    let alt_dir = alt_root.path().join(".alt");

    let t0 = Instant::now();
    let report = import_git(&repo, &alt_dir, "bench", 1).unwrap();
    let import_s = t0.elapsed().as_secs_f64();

    let compact = {
        let mut odb = NativeOdb::open(&alt_dir).unwrap();
        let r = odb.compact().unwrap();
        odb.flush().unwrap();
        r
    };
    let alt_bytes = dir_bytes(&alt_dir);

    let mib = |b: u64| b as f64 / (1 << 20) as f64;
    eprintln!();
    eprintln!("M7-B2 large-files bench (initial import)");
    eprintln!("  working tree:    {:.1} MiB", mib(working_bytes));
    eprintln!("  .git/objects:    {:.1} MiB", mib(git_bytes_before),);
    eprintln!(
        "  .alt store:      {:.1} MiB  ({:.2}x git)",
        mib(alt_bytes),
        alt_bytes as f64 / git_bytes_before as f64,
    );
    eprintln!(
        "  import:          {:.2}s ({:.1} MiB/s working, {} objects, {} lineage deltas)",
        import_s,
        mib(working_bytes) / import_s,
        report.objects_seen,
        report.lineage_deltas,
    );
    eprintln!(
        "  compaction:      {} -> {} packs, reclaimed {:.1} MiB",
        compact.packs_before,
        compact.packs_after,
        mib(compact.bytes_before.saturating_sub(compact.bytes_after)),
    );

    // ---- "modify + re-import" pass: append to dataset01 + re-import ----
    // Clone the corpus to a tempdir so we don't pollute the canonical
    // fixture, then add a fresh commit that modifies one dataset.
    let work_root = tempfile::tempdir().unwrap();
    let work = work_root.path().join("large-files");
    let cp = Command::new("cp")
        .args(["-R", corpus.to_str().unwrap(), work.to_str().unwrap()])
        .status()
        .unwrap();
    assert!(cp.success(), "cp -R corpus failed");
    let target = work.join("data/dataset01.dat");
    let mut data = fs::read(&target).unwrap();
    // append 512 KiB of fresh pseudo-random bytes — same pattern the B1
    // builder uses, just with a different seed
    let mut s: u64 = 0xb2_b2_b2_b2_b2_b2_b2_b2u64;
    let start = data.len();
    data.extend(std::iter::repeat_n(0u8, 512 * 1024));
    for chunk in data[start..].chunks_mut(8) {
        s = s.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = s;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^= z >> 31;
        let bytes = z.to_le_bytes();
        let n = chunk.len();
        chunk.copy_from_slice(&bytes[..n]);
    }
    fs::write(&target, &data).unwrap();
    git(&work, &["add", "-A"]);
    git(
        &work,
        &[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "--allow-empty-message",
            "-m",
            "B2 bench: append to dataset01",
        ],
    );

    let git_bytes_after = dir_bytes(&work.join(".git/objects"));
    let repo = Repository::discover(&work).unwrap();
    let t0 = Instant::now();
    let inc = import_git(&repo, &alt_dir, "bench", 2).unwrap();
    let inc_s = t0.elapsed().as_secs_f64();
    {
        let mut odb = NativeOdb::open(&alt_dir).unwrap();
        odb.compact().unwrap();
        odb.flush().unwrap();
    }
    let alt_bytes_after = dir_bytes(&alt_dir);
    let git_delta = git_bytes_after.saturating_sub(git_bytes_before);
    let alt_delta = alt_bytes_after.saturating_sub(alt_bytes);

    eprintln!();
    eprintln!("M7-B2 large-files bench (modify + re-import)");
    eprintln!(
        "  new git objects: {:.2} MiB (Δ over baseline)",
        mib(git_delta),
    );
    eprintln!(
        "  new alt bytes:   {:.2} MiB (Δ over baseline)",
        mib(alt_delta),
    );
    eprintln!(
        "  re-import:       {:.2}s, {} new objects (out of {} seen), {} new lineage deltas",
        inc_s, inc.objects_new, inc.objects_seen, inc.lineage_deltas,
    );
    eprintln!(
        "  incremental ratio: alt Δ / git Δ = {:.2}x",
        alt_delta as f64 / git_delta as f64,
    );
}
