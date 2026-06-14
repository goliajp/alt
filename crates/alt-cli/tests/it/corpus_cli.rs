//! Corpus-scale byte-exactness: full-history `log --pretty=raw` (and
//! friends) diffed against git on every real repository under
//! `$ALT_CORPUS` that has a resolvable HEAD — first over the .git
//! backend, then over a native .alt store imported from it (the M1
//! matrix re-run on the M2 backend).

use std::path::Path;
use std::process::{Command, Output};

fn run(bin: &str, repo: &Path, args: &[&str]) -> Output {
    Command::new(bin)
        .current_dir(repo)
        .env("ALT_NO_DAEMON", "1")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args(args)
        .output()
        .unwrap()
}

fn assert_same_split(alt_cwd: &Path, git_cwd: &Path, args: &[&str]) {
    let alt = run(env!("CARGO_BIN_EXE_alt"), alt_cwd, args);
    let git = run("git", git_cwd, args);
    assert!(git.status.success(), "git {args:?} in {git_cwd:?}");
    assert!(
        alt.status.success(),
        "alt {args:?} in {alt_cwd:?}: {}",
        String::from_utf8_lossy(&alt.stderr)
    );
    assert!(
        alt.stdout == git.stdout,
        "stdout mismatch for {args:?} in {alt_cwd:?} (alt {} bytes vs git {} bytes)",
        alt.stdout.len(),
        git.stdout.len()
    );
}

/// One repo's command sweep, alt reading `alt_cwd`, git reading `git_cwd`.
fn sweep(alt_cwd: &Path, git_cwd: &Path, head: &str) {
    assert_same_split(alt_cwd, git_cwd, &["rev-parse", "HEAD"]);
    for flag in ["-t", "-s", "-p"] {
        assert_same_split(alt_cwd, git_cwd, &["cat-file", flag, head]);
    }
    assert_same_split(alt_cwd, git_cwd, &["log", "--pretty=raw"]);
    assert_same_split(alt_cwd, git_cwd, &["log", "--pretty=oneline"]);
    assert_same_split(alt_cwd, git_cwd, &["log", "--pretty=raw", "-n", "50"]);
}

#[test]
#[ignore = "needs $ALT_CORPUS pointing at a directory of git repos"]
fn corpus_cli_matches_git() {
    let corpus = std::env::var("ALT_CORPUS").expect("set ALT_CORPUS to the corpus directory");
    let mut swept = 0;
    for entry in std::fs::read_dir(&corpus).unwrap() {
        let repo = entry.unwrap().path();
        if !repo.join(".git").is_dir() {
            continue;
        }
        // repos without a resolvable HEAD (e.g. unpack-only copies) are
        // object-store corpus, not CLI corpus
        if !run("git", &repo, &["rev-parse", "HEAD"]).status.success() {
            continue;
        }
        let head_out = run("git", &repo, &["rev-parse", "HEAD"]).stdout;
        let head = String::from_utf8(head_out).unwrap();
        let head = head.trim();

        sweep(&repo, &repo, head);
        println!("{}: full-history log byte-exact (.git)", repo.display());

        // the M1 matrix re-run on the native backend
        let alt_root = tempfile::tempdir().unwrap();
        let import = run(
            env!("CARGO_BIN_EXE_alt"),
            &repo,
            &["import", alt_root.path().to_str().unwrap()],
        );
        assert!(
            import.status.success(),
            "alt import {repo:?}: {}",
            String::from_utf8_lossy(&import.stderr)
        );
        sweep(alt_root.path(), &repo, head);
        println!("{}: full-history log byte-exact (.alt)", repo.display());
        swept += 1;
    }
    assert!(swept > 0, "no usable repositories under {corpus}");
}
