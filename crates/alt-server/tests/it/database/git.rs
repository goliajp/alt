use super::*;
use base64::Engine;
use std::{
    path::Path,
    process::{Command, Output},
};

fn git(directory: &Path, token: &str, args: &[&str]) -> Output {
    let auth = base64::engine::general_purpose::STANDARD.encode(format!("alt:{token}"));
    Command::new("git")
        .args(args)
        .current_dir(directory)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap())
        .env("LC_ALL", "C")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "http.extraHeader")
        .env("GIT_CONFIG_VALUE_0", format!("Authorization: Basic {auth}"))
        .output()
        .unwrap()
}
pub(super) fn ok(directory: &Path, token: &str, args: &[&str]) -> Vec<u8> {
    let output = git(directory, token, args);
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}
pub(super) fn identity(directory: &Path, token: &str) {
    ok(directory, token, &["config", "user.name", "Original Actor"]);
    ok(
        directory,
        token,
        &["config", "user.email", "original@example.test"],
    );
}
pub(super) fn text(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes).unwrap().trim().to_owned()
}

#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn real_git_atomic_push_clone_incremental_fetch_notes_tags_and_default_branch() {
    let f = Fixture::new();
    let server = Running::start(&f);
    let temp = tempfile::tempdir().unwrap();
    let first = temp.path().join("first");
    std::fs::create_dir(&first).unwrap();
    let url = format!("{}/git/repository", server.url);
    ok(&first, &f.token, &["init", "-q", "--initial-branch=main"]);
    identity(&first, &f.token);
    let source = (0..2000)
        .map(|i| format!("source line {i:04}: original independent bytes\n"))
        .collect::<String>();
    std::fs::write(first.join("source.txt"), &source).unwrap();
    let binary = [0u8, 255, 1, 0, 13, 10, 200];
    std::fs::write(first.join("asset.bin"), binary).unwrap();
    std::os::unix::fs::symlink("source.txt", first.join("source-link")).unwrap();
    ok(&first, &f.token, &["add", "."]);
    ok(&first, &f.token, &["commit", "-qm", "original source"]);
    let initial = text(ok(&first, &f.token, &["rev-parse", "HEAD"]));
    ok(
        &first,
        &f.token,
        &["tag", "-a", "v1", "-m", "release source"],
    );
    ok(
        &first,
        &f.token,
        &["notes", "add", "-m", "original delivery facts", "HEAD"],
    );
    ok(&first, &f.token, &["branch", "aaa"]);
    ok(&first, &f.token, &["remote", "add", "origin", &url]);
    ok(
        &first,
        &f.token,
        &[
            "push",
            "--atomic",
            "origin",
            "main",
            "aaa",
            "refs/tags/v1",
            "refs/notes/commits",
        ],
    );
    assert_eq!(f.counts(), (4, 1, 1));
    ok(temp.path(), &f.token, &["clone", "--quiet", &url, "second"]);
    let second = temp.path().join("second");
    assert_eq!(
        text(ok(&second, &f.token, &["symbolic-ref", "HEAD"])),
        "refs/heads/main"
    );
    assert_eq!(text(ok(&second, &f.token, &["rev-parse", "HEAD"])), initial);
    assert_eq!(std::fs::read(second.join("asset.bin")).unwrap(), binary);
    assert_eq!(
        std::fs::read_link(second.join("source-link")).unwrap(),
        Path::new("source.txt")
    );
    assert_eq!(
        text(ok(&second, &f.token, &["rev-parse", "v1^{}"])),
        initial
    );
    std::fs::write(
        first.join("source.txt"),
        format!("{source}incremental reviewed change\n"),
    )
    .unwrap();
    ok(&first, &f.token, &["add", "source.txt"]);
    ok(&first, &f.token, &["commit", "-qm", "incremental source"]);
    let next = text(ok(&first, &f.token, &["rev-parse", "HEAD"]));
    ok(&first, &f.token, &["push", "origin", "main"]);
    ok(&second, &f.token, &["fetch", "origin"]);
    ok(&second, &f.token, &["merge", "--ff-only", "origin/main"]);
    assert_eq!(text(ok(&second, &f.token, &["rev-parse", "HEAD"])), next);
    assert_eq!(
        std::fs::read(first.join("source.txt")).unwrap(),
        std::fs::read(second.join("source.txt")).unwrap()
    );
    ok(
        &second,
        &f.token,
        &["fetch", "origin", "refs/notes/commits:refs/notes/commits"],
    );
    assert_eq!(
        ok(&first, &f.token, &["notes", "show", &initial]),
        ok(&second, &f.token, &["notes", "show", &initial])
    );
    ok(&second, &f.token, &["fsck", "--strict", "--full"]);
    assert_eq!(f.counts(), (4, 2, 2));
}

#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn real_git_protected_branch_rejects_entire_atomic_push_and_force_without_review() {
    let mut f = Fixture::new();
    let server = Running::start(&f);
    let temp = tempfile::tempdir().unwrap();
    ok(
        temp.path(),
        &f.token,
        &["init", "-q", "--initial-branch=main"],
    );
    identity(temp.path(), &f.token);
    std::fs::write(temp.path().join("file"), "base\n").unwrap();
    ok(temp.path(), &f.token, &["add", "file"]);
    ok(temp.path(), &f.token, &["commit", "-qm", "base"]);
    let base = text(ok(temp.path(), &f.token, &["rev-parse", "HEAD"]));
    let url = format!("{}/git/repository", server.url);
    ok(temp.path(), &f.token, &["remote", "add", "origin", &url]);
    ok(temp.path(), &f.token, &["push", "origin", "main"]);
    f.store
        .set_policies(
            &f.credential,
            "repository",
            &[alt_repository_service::RefPolicy {
                refs: RefSelector::Exact("refs/heads/main".into()),
                allow_force: false,
                allow_delete: false,
                require_review: true,
            }],
        )
        .unwrap();
    std::fs::write(temp.path().join("file"), "new source\n").unwrap();
    ok(temp.path(), &f.token, &["add", "file"]);
    ok(temp.path(), &f.token, &["commit", "-qm", "new"]);
    let rejected = git(
        temp.path(),
        &f.token,
        &[
            "push",
            "--atomic",
            "origin",
            "main",
            "HEAD:refs/heads/topic",
        ],
    );
    assert!(!rejected.status.success());
    let info = f
        .store
        .repository_info(&f.credential, "repository")
        .unwrap();
    assert_eq!(info.refs["refs/heads/main"], base);
    assert!(!info.refs.contains_key("refs/heads/topic"));
    assert_eq!(f.counts(), (1, 1, 1));
    f.store
        .set_policies(
            &f.credential,
            "repository",
            &[alt_repository_service::RefPolicy {
                refs: RefSelector::Exact("refs/heads/main".into()),
                allow_force: false,
                allow_delete: false,
                require_review: false,
            }],
        )
        .unwrap();
    ok(temp.path(), &f.token, &["push", "origin", "main"]);
    let next = text(ok(temp.path(), &f.token, &["rev-parse", "HEAD"]));
    let rejected = git(
        temp.path(),
        &f.token,
        &[
            "push",
            "--force",
            "origin",
            &format!("{base}:refs/heads/main"),
        ],
    );
    assert!(!rejected.status.success());
    assert_eq!(
        f.store
            .repository_info(&f.credential, "repository")
            .unwrap()
            .refs["refs/heads/main"],
        next
    );
    assert_eq!(f.counts(), (1, 2, 2));
    let rejected = git(
        temp.path(),
        &f.token,
        &["push", "origin", ":refs/heads/main"],
    );
    assert!(!rejected.status.success());
    assert_eq!(f.counts(), (1, 2, 2));
}
