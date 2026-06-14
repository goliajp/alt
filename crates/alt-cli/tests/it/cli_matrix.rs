//! The byte-exactness matrix: every alt command output is diffed against
//! git's, hermetically (no system/global git config).

use std::path::Path;
use std::process::{Command, Output};

use alt_testutil as common;

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

/// Runs the same args through alt (in `alt_cwd`) and git (in `git_cwd`);
/// both must succeed with identical stdout bytes. The two directories
/// differ exactly when alt reads a native .alt store imported from the
/// git repository (no coexistence: the stores live apart).
fn assert_same_split(alt_cwd: &Path, git_cwd: &Path, args: &[&str]) {
    let alt = run(env!("CARGO_BIN_EXE_alt"), alt_cwd, args);
    let git = run("git", git_cwd, args);
    assert!(
        git.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&git.stderr)
    );
    assert!(
        alt.status.success(),
        "alt {args:?}: {}",
        String::from_utf8_lossy(&alt.stderr)
    );
    assert_eq!(
        alt.stdout.as_bstr(),
        git.stdout.as_bstr(),
        "stdout mismatch for {args:?} in {alt_cwd:?}"
    );
}

use bstr::ByteSlice;

fn matrix(repo: &Path) {
    matrix_split(repo, repo)
}

/// The same matrix with alt reading `alt_cwd` while git reads `git_cwd`.
fn matrix_split(alt_cwd: &Path, git_cwd: &Path) {
    let repo = git_cwd;
    let head = common::git(repo, &["rev-parse", "HEAD"]);
    let tree = common::git(repo, &["rev-parse", "HEAD^{tree}"]);
    let blob = common::git(repo, &["rev-parse", "HEAD:a.txt"]);
    let (head, tree, blob) = (head.trim(), tree.trim(), blob.trim());

    for rev in ["HEAD", "main", "feat", "v0", "refs/heads/main", head] {
        assert_same_split(alt_cwd, git_cwd, &["rev-parse", rev]);
    }
    for oid in [head, tree, blob] {
        for flag in ["-t", "-s", "-p"] {
            assert_same_split(alt_cwd, git_cwd, &["cat-file", flag, oid]);
        }
    }
    // annotated tag: type/size/payload without peeling
    for flag in ["-t", "-s", "-p"] {
        assert_same_split(alt_cwd, git_cwd, &["cat-file", flag, "v0"]);
    }
    for extra in [
        &["--pretty=raw"][..],
        &["--pretty=raw", "-n", "3"],
        &["--pretty=raw", "-n", "1"],
        &["--pretty=oneline"],
        &["--pretty=oneline", "-n", "2"],
        &["--pretty=raw", "feat"],
        &["--pretty=raw", "v0"],
    ] {
        let mut args = vec!["log"];
        args.extend_from_slice(extra);
        assert_same_split(alt_cwd, git_cwd, &args);
    }
}

#[test]
fn matrix_matches_git() {
    for (object_format, ref_format) in
        [("sha1", "files"), ("sha256", "files"), ("sha1", "reftable")]
    {
        let tmp = tempfile::tempdir().unwrap();
        common::make_repo_opts(tmp.path(), object_format, ref_format);
        matrix(tmp.path());
        common::pack_repo(tmp.path());
        matrix(tmp.path());

        // the same matrix over the native backend: import via the real
        // CLI into a clean directory, then alt reads .alt while git
        // still reads the original repository
        let alt_root = tempfile::tempdir().unwrap();
        let import = run(
            env!("CARGO_BIN_EXE_alt"),
            tmp.path(),
            &["import", alt_root.path().to_str().unwrap()],
        );
        assert!(
            import.status.success(),
            "alt import: {}",
            String::from_utf8_lossy(&import.stderr)
        );
        matrix_split(alt_root.path(), tmp.path());
    }
}
