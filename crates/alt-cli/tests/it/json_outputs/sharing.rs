use super::Scene;
use std::path::Path;
use std::process::Command;

fn git(directory: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(directory)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@test")
        .env("GIT_COMMITTER_NAME", "fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@test")
        .env("GIT_AUTHOR_DATE", "2020-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2020-01-01T00:00:00Z")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn remotes_push_fetch_and_clone() {
    let mut s = Scene::new("remotes-push-fetch-clone");
    // the size of a pack follows the commit times inside it
    s.volatile(&["pack_bytes"]);
    s.volatile_between("ref(s), ", " byte(s) of packfile");
    s.env("ALT_NO_CREDENTIAL_HELPER", "1");
    s.cd("served");
    git(&s.path(""), &["init", "-q", "--bare", "-b", "main", "."]);
    git(
        &s.path(""),
        &["config", "receive.denyNonFastForwards", "false"],
    );
    let url = crate::wire_test_server::spawn(s.path(""));
    s.alias(&url, "server");
    s.cd("repo");
    s.setup(&["init", "."]);
    s.write("a.txt", "hello\n");
    s.write("sub/b.txt", "nested\n");
    s.setup(&["add", "."]);
    s.setup(&["commit", "-m", "first"]);
    s.json(&["remote", "add", "origin", &url, "--json"]);
    s.json(&["remote", "add", "origin", &url, "--json"]);
    s.json(&[
        "remote",
        "add",
        "other",
        "https://example.com/日本語.git",
        "--json",
    ]);
    s.json(&["remote", "list", "--json"]);
    s.json(&["remote", "remove", "other", "--json"]);
    s.json(&["fetch", "origin", "--json"]);
    s.json(&["push", "origin", "--json"]);
    s.json(&["push", "origin", "--json"]);
    s.json(&["fetch", "origin", "--json"]);
    s.json(&["branch", "--json"]);
    s.setup(&["tag", "v1"]);
    s.json(&["push", "origin", "refs/tags/v1:refs/tags/v1", "--json"]);
    s.cd("");
    s.json(&["clone", &url, "copy", "--json"]);
    s.json(&["clone", &url, "copy", "--json"]);
    s.cd("copy");
    s.json(&["status", "--json"]);
    s.json(&["remote", "list", "--json"]);
    s.write("a.txt", "from the copy\n");
    s.setup(&["add", "."]);
    s.setup(&["commit", "-m", "second"]);
    s.json(&["push", "origin", "--json"]);
    s.cd("repo");
    s.json(&["fetch", "origin", "--json"]);
    s.json(&["fetch", "origin", "--json"]);
    s.json(&["branch", "--json"]);
    s.write("a.txt", "diverged\n");
    s.setup(&["add", "."]);
    s.setup(&["commit", "-m", "diverged"]);
    s.json(&["push", "origin", "--json"]);
    s.json(&["push", "origin", "-f", "--json"]);
    s.json(&["push", "origin", "nonesuch", "--json"]);
    s.json(&["fetch", "origin", "refs/heads/nonesuch", "--json"]);
    s.json(&["branch", "-d", "origin/main", "-r", "--json"]);
    s.json(&["op-log", "-n", "4", "--json"]);
    s.finish();
}

#[test]
fn identities_and_signatures() {
    let mut s = Scene::new("identities-signatures");
    s.volatile(&["fingerprint"]);
    s.cd("repo");
    s.setup(&["init", "."]);
    s.json(&["identity", "init", "alice", "--json"]);
    s.json(&["identity", "init", "alice", "--json"]);
    s.json(&["identity", "init", "bob", "--json"]);
    s.json(&["identity", "list", "--json"]);
    let key = s.text(".alt/identity/alice.pub");
    s.json(&["identity", "trust", "alice", &key, "--json"]);
    s.write("not-a-key.pub", "garbage\n");
    s.json(&["identity", "trust", "carol", "not-a-key.pub", "--json"]);
    s.json(&["identity", "list", "--json"]);
    s.write(".alt/sign-policy", "enabled = true\nprincipal = alice\n");
    s.env("ALT_PRINCIPAL_ID", "alice");
    s.write("a.txt", "signed\n");
    s.json(&["add", ".", "--json"]);
    s.json(&["commit", "-m", "alice signed", "--json"]);
    s.json(&["verify", "--json"]);
    s.json(&["op-log", "--verify", "--json"]);
    s.json(&["status", "--json"]);
    s.env("ALT_PRINCIPAL_ID", "bob");
    s.env("ALT_PRINCIPAL_KIND", "agent");
    s.env("ALT_SESSION_ID", "session-7");
    s.write("b.txt", "by bob\n");
    s.json(&["add", ".", "--json"]);
    s.json(&["commit", "-m", "bob unsigned", "--json"]);
    s.json(&["verify", "--json"]);
    s.json(&["verify", "-n", "1", "--json"]);
    s.json(&["op-log", "--verify", "--json"]);
    s.json(&["status", "--json"]);
    s.finish();
}

#[test]
fn lfs_pointers() {
    let mut s = Scene::new("lfs-pointers");
    s.cd("repo");
    s.setup(&["init", "."]);
    s.write(
        ".gitattributes",
        "*.bin filter=lfs diff=lfs merge=lfs -text\n",
    );
    s.write("w.bin", vec![9u8; 50_000]);
    s.write("small.txt", "plain\n");
    s.json(&["add", ".", "--json"]);
    s.json(&["status", "--json"]);
    s.json(&["diff", "--cached", "--json"]);
    s.json(&["commit", "-m", "weights", "--json"]);
    s.json(&["show", "--json"]);
    s.write("w.bin", vec![7u8; 60_000]);
    s.json(&["status", "--json"]);
    s.json(&["diff", "--json"]);
    s.json(&["add", ".", "--json"]);
    s.json(&["commit", "-m", "new weights", "--json"]);
    s.json(&["show", "--json"]);
    s.json(&["log", "--json"]);
    s.json(&["lfs", "ls"]);
    s.finish();
}

#[test]
fn intake_of_a_git_repository() {
    let mut s = Scene::new("intake");
    s.volatile(&["version_stdout_hex", "config_hex"]);
    s.cd("source");
    let source = s.path("");
    git(&source, &["init", "-q", "-b", "main"]);
    s.write("file.txt", "committed\n");
    git(&source, &["add", "."]);
    git(&source, &["commit", "-q", "-m", "baseline"]);
    s.write("file.txt", "preexisting dirty\n");
    s.write(".env.local", "excluded\n");
    s.cd("");
    let (source, target) = (s.text("source"), s.text("native"));
    s.json(&["intake", &source, &target, "--json"]);
    s.json(&["intake", &source, &target, "--json"]);
    s.cd("native");
    s.json(&["status", "--json"]);
    s.json(&["log", "--json"]);
    s.cd("");
    git(
        Path::new(&source),
        &["config", "include.path", "/never-read"],
    );
    let rejected = s.text("rejected");
    s.json(&["intake", &source, &rejected, "--json"]);
    s.finish();
}
