//! The cost the daemon amortizes (D3). Every direct `alt` command pays a full
//! [`Store::open`] — mmap the altpacks, read `map.alt`, replay the op log; a
//! daemon holds the store open and instead pays only [`Store::refresh`] per
//! request (a tail catch-up that reads nothing new when idle). This benchmarks
//! the two against a fixture repo so the per-request saving is visible:
//! `open` is what each fresh process pays, `refresh` is the daemon's per-request
//! cost, and their difference is what routing a read through the daemon saves.

use std::path::Path;
use std::process::Command;

use alt_cli::native::{Store, resolve_workspace};
use alt_repo::Repository;
use criterion::{Criterion, criterion_group, criterion_main};

/// Builds a repo with `commits` commits via the real `alt` binary (setup runs
/// once, outside the timed loop), returning the live tempdir and its `.alt`.
fn fixture(commits: usize) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    run(root, &["init", "."]);
    for i in 0..commits {
        std::fs::write(root.join("f.txt"), format!("rev {i}\n")).unwrap();
        run(root, &["add", "."]);
        run(root, &["commit", "-m", &format!("c{i}")]);
    }
    let (alt_dir, _) = resolve_workspace(root, None).unwrap();
    (dir, alt_dir)
}

fn run(cwd: &Path, args: &[&str]) {
    let status = Command::new(env!("CARGO_BIN_EXE_alt"))
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "bench")
        .env("GIT_AUTHOR_EMAIL", "b@e")
        .env("ALT_RELAXED_DURABILITY", "1") // setup speed only; not timed
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "alt {args:?} failed");
}

fn bench(c: &mut Criterion) {
    let (dir, alt_dir) = fixture(200);
    let root = dir.path();

    // native reads (status/branch/diff): cold open vs the daemon's per-request
    // store catch-up
    c.bench_function("store_open", |b| {
        b.iter(|| Store::open(alt_dir.clone()).unwrap());
    });
    let mut store = Store::open(alt_dir.clone()).unwrap();
    c.bench_function("store_refresh", |b| {
        b.iter(|| store.refresh().unwrap());
    });

    // `log` (git-layer): cold Repository discover vs the daemon's per-request
    // repository catch-up — the open it now amortizes too
    c.bench_function("repo_open", |b| {
        b.iter(|| Repository::discover(root).unwrap());
    });
    let mut repo = Repository::discover(root).unwrap();
    c.bench_function("repo_refresh", |b| {
        b.iter(|| repo.refresh().unwrap());
    });
}

criterion_group!(benches, bench);
criterion_main!(benches);
