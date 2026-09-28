//! A git repository nested in an alt working tree is its own repository: the
//! nearest one wins, as in git.

use std::path::Path;
use std::process::{Command, Output};

fn run(bin: &str, cwd: &Path, args: &[&str]) -> Output {
    Command::new(bin)
        .current_dir(cwd)
        .env("ALT_NO_DAEMON", "1")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .env("GIT_COMMITTER_NAME", "tester")
        .env("GIT_COMMITTER_EMAIL", "t@e")
        .args(args)
        .output()
        .unwrap()
}

fn ok(o: Output) -> String {
    assert!(
        o.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout).unwrap()
}

#[test]
fn commands_inside_a_nested_git_repo_read_that_repo() {
    let dir = tempfile::tempdir().unwrap();
    let outer = dir.path();
    let alt = env!("CARGO_BIN_EXE_alt");
    ok(run(alt, outer, &["init", "."]));
    std::fs::write(outer.join("o.txt"), "outer\n").unwrap();
    ok(run(alt, outer, &["add", "."]));
    ok(run(alt, outer, &["commit", "-m", "outer"]));
    let outer_head = ok(run(alt, outer, &["rev-parse", "HEAD"]));

    let inner = outer.join("vendor/lib");
    std::fs::create_dir_all(&inner).unwrap();
    ok(run("git", &inner, &["init", "-q", "-b", "main"]));
    std::fs::write(inner.join("i.txt"), "inner\n").unwrap();
    ok(run("git", &inner, &["add", "."]));
    ok(run("git", &inner, &["commit", "-qm", "inner"]));
    let inner_head = ok(run("git", &inner, &["rev-parse", "HEAD"]));

    assert_eq!(ok(run(alt, &inner, &["rev-parse", "HEAD"])), inner_head);
    assert_eq!(
        ok(run(alt, &inner.join(".."), &["rev-parse", "HEAD"])),
        outer_head
    );
    assert_ne!(inner_head, outer_head);
    // the outer tree does not take the nested repository in as content
    let st = ok(run(alt, outer, &["status"]));
    assert!(!st.contains("i.txt"), "{st}");
}
