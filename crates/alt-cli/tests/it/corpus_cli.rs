//! Corpus-scale byte-exactness: full-history `log --pretty=raw` (and
//! friends) diffed against git on every real repository under
//! `$ALT_CORPUS` that has a resolvable HEAD.

use std::path::Path;
use std::process::{Command, Output};

fn run(bin: &str, repo: &Path, args: &[&str]) -> Output {
    Command::new(bin)
        .current_dir(repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args(args)
        .output()
        .unwrap()
}

fn assert_same(repo: &Path, args: &[&str]) {
    let alt = run(env!("CARGO_BIN_EXE_alt"), repo, args);
    let git = run("git", repo, args);
    assert!(git.status.success(), "git {args:?} in {repo:?}");
    assert!(
        alt.status.success(),
        "alt {args:?} in {repo:?}: {}",
        String::from_utf8_lossy(&alt.stderr)
    );
    assert!(
        alt.stdout == git.stdout,
        "stdout mismatch for {args:?} in {repo:?} (alt {} bytes vs git {} bytes)",
        alt.stdout.len(),
        git.stdout.len()
    );
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

        assert_same(&repo, &["rev-parse", "HEAD"]);
        for flag in ["-t", "-s", "-p"] {
            assert_same(&repo, &["cat-file", flag, head]);
        }
        assert_same(&repo, &["log", "--pretty=raw"]);
        assert_same(&repo, &["log", "--pretty=oneline"]);
        assert_same(&repo, &["log", "--pretty=raw", "-n", "50"]);
        println!("{}: full-history log byte-exact", repo.display());
        swept += 1;
    }
    assert!(swept > 0, "no usable repositories under {corpus}");
}
