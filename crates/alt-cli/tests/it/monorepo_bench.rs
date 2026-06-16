//! M8-A2 monorepo bench: side-by-side wall-time on the M8-A1 synth
//! corpus for the hot CLI commands an agent invokes on every interaction
//! — status / commit / diff / log / log -p / branch / cat-file — through
//! both the .alt store and the source .git, so the gap between alt and
//! git native is visible per command without a third-party comparator.
//!
//! Numbers are reported, the plan doc records them; **no hard threshold
//! assertions** here — A3 is the magnification step, and the bench needs
//! to be honest about whatever the current numbers are.
//!
//! Gated like the other bench: `ALT_BENCH=1` on top of `--ignored`.
//! Defaults to `.claude/corpus/monorepo`; override via
//! `ALT_MONOREPO_CORPUS=<dir>`. Skip cleanly with a helpful note if the
//! corpus isn't built — `scripts/build-monorepo-corpus.sh` provisions it.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use alt_cli::native::{Identity, OpenRepo};
use alt_import::import_git;
use alt_repo::Repository;

const ITERATIONS: usize = 5;

fn alt_bin() -> &'static str {
    env!("CARGO_BIN_EXE_alt")
}

/// Run one command in `cwd` and return its wall-clock time + stdout/stderr.
/// `no_daemon` controls whether alt's daemon path is allowed; git ignores it.
fn timed(cwd: &Path, cmd: &str, args: &[&str], no_daemon: bool) -> (Duration, Output) {
    let t0 = Instant::now();
    let mut c = Command::new(cmd);
    c.current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "bench")
        .env("GIT_AUTHOR_EMAIL", "bench@test")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if no_daemon {
        c.env("ALT_NO_DAEMON", "1");
    }
    let out = c.output().unwrap();
    (t0.elapsed(), out)
}

/// p50 / p99 over `iterations` runs; warm-up is the first run, since the
/// first invocation pays page-cache / fork overhead the next ones don't.
fn measure(
    cwd: &Path,
    cmd: &str,
    args: &[&str],
    iterations: usize,
    no_daemon: bool,
) -> (Duration, Duration) {
    // warm-up — discard, but assert success so a broken command doesn't
    // silently report a fast number
    let (_, warm) = timed(cwd, cmd, args, no_daemon);
    assert!(
        warm.status.success(),
        "warm-up {cmd} {args:?} failed: stderr={} stdout={}",
        String::from_utf8_lossy(&warm.stderr),
        String::from_utf8_lossy(&warm.stdout),
    );
    let mut samples = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let (d, out) = timed(cwd, cmd, args, no_daemon);
        assert!(out.status.success(), "{cmd} {args:?} failed mid-bench");
        samples.push(d);
    }
    samples.sort();
    let p50 = samples[samples.len() / 2];
    let p99 = samples[(samples.len() * 99 / 100).min(samples.len() - 1)];
    (p50, p99)
}

/// Copy `src`'s `.git` to `<dst>/.git` so we can run `alt import` against
/// a clean source dir without entangling alt's `.alt` with the corpus'
/// `.git` (the project rule: never coexist `.git` and `.alt` in the same
/// dir).
fn cp_git_dir(src: &Path, dst: &Path) {
    let status = Command::new("cp")
        .args([
            "-R",
            src.join(".git").to_str().unwrap(),
            dst.join(".git").to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert!(status.success(), "cp -R failed");
}

/// Provision a working `.alt`-backed copy of the corpus next to a
/// `git`-backed one, with parallel working trees materialised, so each
/// timed command sees a realistic monorepo on each side.
struct Workspaces {
    alt: PathBuf,
    git: PathBuf,
    // hold the tempdir so it's not cleaned up under our feet
    _temp: tempfile::TempDir,
}

fn setup(corpus: &Path) -> Workspaces {
    let temp = tempfile::tempdir().unwrap();
    let git_work = temp.path().join("git-work");
    let alt_work = temp.path().join("alt-work");
    std::fs::create_dir_all(&git_work).unwrap();
    std::fs::create_dir_all(&alt_work).unwrap();

    // git side: copy the corpus's .git, then `git reset --hard main`
    // materialises the working tree out of it.
    cp_git_dir(corpus, &git_work);
    let st = Command::new("git")
        .current_dir(&git_work)
        .args(["reset", "--hard", "main", "-q"])
        .status()
        .unwrap();
    assert!(st.success(), "git reset --hard main failed");

    // alt side: a parallel source dir holds the .git so `alt import`
    // doesn't try to write `.alt` next to it. Then materialise via the
    // library's `materialise_head` — clone normally does this; the
    // `alt import` CLI doesn't, so we drive the library directly.
    let alt_source = temp.path().join("alt-source");
    std::fs::create_dir_all(&alt_source).unwrap();
    cp_git_dir(corpus, &alt_source);

    let repo = Repository::discover(&alt_source).unwrap();
    import_git(&repo, &alt_work.join(".alt"), "bench", 1).unwrap();

    let mut open = OpenRepo::discover(&alt_work, None, Identity::from_env()).unwrap();
    open.repo().materialise_head().unwrap();
    drop(open); // release any held write lock before the bench commands run

    Workspaces {
        alt: alt_work,
        git: git_work,
        _temp: temp,
    }
}

fn fmt_dur(d: Duration) -> String {
    let ms = d.as_secs_f64() * 1000.0;
    if ms >= 1000.0 {
        format!("{:.2}s", ms / 1000.0)
    } else {
        format!("{:.1}ms", ms)
    }
}

#[test]
#[ignore = "bench: set ALT_BENCH=1, run by hand in release"]
fn monorepo_bench_core_commands() {
    if std::env::var("ALT_BENCH").as_deref() != Ok("1") {
        eprintln!("monorepo_bench: skipped (set ALT_BENCH=1 to run)");
        return;
    }
    let corpus = std::env::var("ALT_MONOREPO_CORPUS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(".claude/corpus/monorepo"));
    if !corpus.join(".git").is_dir() {
        eprintln!(
            "monorepo_bench: {} does not look like a git repo — run \
             scripts/build-monorepo-corpus.sh first",
            corpus.display()
        );
        return;
    }

    eprintln!();
    eprintln!("M8-A2 monorepo bench (setup...)");
    let t0 = Instant::now();
    let ws = setup(&corpus);
    eprintln!("  setup took {}", fmt_dur(t0.elapsed()));

    // Drive both backends through the same matrix; each command must be
    // semantically identical (status, log, etc.).
    let cases: &[(&str, &[&str], &[&str])] = &[
        ("status (clean)", &["status"], &["status", "--porcelain"]),
        (
            "log -n 1 --pretty=oneline",
            &["log", "-n", "1", "--pretty=oneline"],
            &["log", "-n", "1", "--pretty=oneline"],
        ),
        (
            "log -n 100 --pretty=oneline",
            &["log", "-n", "100", "--pretty=oneline"],
            &["log", "-n", "100", "--pretty=oneline"],
        ),
        (
            "log -n 1000 --pretty=oneline",
            &["log", "-n", "1000", "--pretty=oneline"],
            &["log", "-n", "1000", "--pretty=oneline"],
        ),
        (
            "log -p -n 5",
            &["log", "-p", "-n", "5", "--pretty=oneline"],
            &["log", "-p", "-n", "5", "--pretty=oneline"],
        ),
        ("diff (clean)", &["diff"], &["diff"]),
        ("branch (list)", &["branch"], &["branch"]),
        (
            "cat-file -p HEAD",
            &["cat-file", "-p", "HEAD"],
            &["cat-file", "-p", "HEAD"],
        ),
    ];

    eprintln!();
    eprintln!(
        "| command | alt p50 (no-daemon) | alt p50 (daemon) | git p50 | alt/git no-daemon | alt/git daemon |"
    );
    eprintln!("|---|---|---|---|---|---|");
    for (label, alt_args, git_args) in cases {
        let (a_nodae_50, _) = measure(&ws.alt, alt_bin(), alt_args, ITERATIONS, true);
        let (a_dae_50, _) = measure(&ws.alt, alt_bin(), alt_args, ITERATIONS, false);
        let (git50, _) = measure(&ws.git, "git", git_args, ITERATIONS, false);
        let r_nodae = a_nodae_50.as_secs_f64() / git50.as_secs_f64();
        let r_dae = a_dae_50.as_secs_f64() / git50.as_secs_f64();
        eprintln!(
            "| {label} | {} | {} | {} | {r_nodae:.2}x | {r_dae:.2}x |",
            fmt_dur(a_nodae_50),
            fmt_dur(a_dae_50),
            fmt_dur(git50),
        );
    }
}
