//! Corpus-scale history editing: on two real repositories, `revert` and an
//! interactive `rebase` (squash) run in alt and in git from the same commit
//! must land on the same trees and messages, and the edited alt history must
//! export to a repository `git fsck --strict` accepts.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn run(bin: &str, repo: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut c = Command::new(bin);
    c.current_dir(repo)
        .env("ALT_NO_DAEMON", "1")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_EDITOR", "true")
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .env("GIT_COMMITTER_NAME", "tester")
        .env("GIT_COMMITTER_EMAIL", "t@e")
        .args(args);
    for (k, v) in env {
        c.env(k, v);
    }
    c.output().unwrap()
}

fn ok(o: Output, what: &str) -> String {
    assert!(
        o.status.success(),
        "{what}: {}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout).unwrap()
}

/// Runs the same command in both, asserting both succeed.
fn both(alt_dir: &Path, git_dir: &Path, args: &[&str], env: &[(&str, &str)]) {
    ok(
        run(env!("CARGO_BIN_EXE_alt"), alt_dir, args, env),
        &format!("alt {args:?}"),
    );
    ok(run("git", git_dir, args, env), &format!("git {args:?}"));
}

/// (tree line, message) of `rev`, asserted equal between the two.
fn same(alt_dir: &Path, git_dir: &Path, rev: &str) -> (String, String) {
    let pick = |cat: String| {
        let (head, msg) = cat.split_once("\n\n").unwrap();
        (head.lines().next().unwrap().to_owned(), msg.to_owned())
    };
    let a = pick(ok(
        run(
            env!("CARGO_BIN_EXE_alt"),
            alt_dir,
            &["cat-file", "-p", rev],
            &[],
        ),
        "alt cat-file",
    ));
    let g = pick(ok(
        run("git", git_dir, &["cat-file", "-p", rev], &[]),
        "git cat-file",
    ));
    assert_eq!(a, g, "{rev} differs in {alt_dir:?}");
    a
}

fn is_merge(git_dir: &Path, rev: &str) -> bool {
    run(
        "git",
        git_dir,
        &["rev-parse", "-q", "--verify", &format!("{rev}^2")],
        &[],
    )
    .status
    .success()
}

/// The two repositories to edit: the preferred small ones when present.
fn pick_repos(corpus: &str) -> Vec<PathBuf> {
    let usable = |p: &Path| {
        p.join(".git").is_dir() && run("git", p, &["rev-parse", "HEAD"], &[]).status.success()
    };
    let mut picked: Vec<PathBuf> = ["gitflow-loose", "libgit2"]
        .iter()
        .map(|n| Path::new(corpus).join(n))
        .filter(|p| usable(p))
        .collect();
    let mut rest: Vec<PathBuf> = std::fs::read_dir(corpus)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| usable(p) && !picked.contains(p))
        .collect();
    rest.sort();
    picked.extend(rest);
    picked.truncate(2);
    picked
}

#[test]
#[ignore = "needs $ALT_CORPUS pointing at a directory of git repos"]
fn corpus_history_edits_match_git() {
    let corpus = std::env::var("ALT_CORPUS").expect("set ALT_CORPUS to the corpus directory");
    let repos = pick_repos(&corpus);
    assert_eq!(
        repos.len(),
        2,
        "need two usable repositories under {corpus}"
    );
    let seq = tempfile::tempdir().unwrap();
    let squash = seq.path().join("squash-second");
    std::fs::write(
        &squash,
        "#!/bin/sh\nawk 'NR==2 {$1=\"squash\"} {print}' \"$1\" > \"$1.tmp\" && mv \"$1.tmp\" \"$1\"\n",
    )
    .unwrap();
    let mut perm = std::fs::metadata(&squash).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perm, 0o755);
    std::fs::set_permissions(&squash, perm).unwrap();

    for repo in repos {
        let work = tempfile::tempdir().unwrap();
        let (a, g) = (work.path().join("alt"), work.path().join("git"));
        ok(
            run(
                "git",
                work.path(),
                &["clone", "-q", repo.to_str().unwrap(), "git"],
                &[],
            ),
            "git clone",
        );
        ok(
            run(
                env!("CARGO_BIN_EXE_alt"),
                &repo,
                &["import", a.to_str().unwrap()],
                &[],
            ),
            "alt import",
        );
        ok(
            run(
                env!("CARGO_BIN_EXE_alt"),
                &a,
                &["switch", "-c", "corpus-edit"],
                &[],
            ),
            "alt switch",
        );
        ok(
            run("git", &g, &["switch", "-q", "-c", "corpus-edit"], &[]),
            "git switch",
        );

        // undo the tip
        let revert: &[&str] = if is_merge(&g, "HEAD") {
            &["revert", "--no-edit", "-m", "1", "HEAD"]
        } else {
            &["revert", "--no-edit", "HEAD"]
        };
        both(&a, &g, revert, &[]);
        let (_, msg) = same(&a, &g, "HEAD");
        assert!(
            msg.starts_with("Revert \"") || msg.starts_with("Reapply \""),
            "{msg}"
        );

        // fold the second of the last three commits into the first
        if !(0..3).any(|i| is_merge(&g, &format!("HEAD~{i}"))) {
            let env = [("GIT_SEQUENCE_EDITOR", squash.to_str().unwrap())];
            both(&a, &g, &["rebase", "-i", "HEAD~3"], &env);
            same(&a, &g, "HEAD");
            same(&a, &g, "HEAD~1");
        }

        let exported = work.path().join("exported");
        ok(
            run(
                env!("CARGO_BIN_EXE_alt"),
                &a,
                &["export", exported.to_str().unwrap()],
                &[],
            ),
            "alt export",
        );
        ok(
            run("git", &exported, &["fsck", "--strict"], &[]),
            "git fsck",
        );
        println!(
            "{}: revert + rebase -i match git; export fsck-clean",
            repo.display()
        );
    }
}
