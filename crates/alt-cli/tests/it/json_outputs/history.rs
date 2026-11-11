use super::Scene;

fn base(name: &'static str) -> Scene {
    let mut s = Scene::new(name);
    s.cd("repo");
    s.setup(&["init", "."]);
    s
}

fn commit(s: &mut Scene, path: &str, contents: &str, message: &str) {
    s.write(path, contents);
    s.setup(&["add", path]);
    s.setup(&["commit", "-m", message]);
}

#[test]
fn empty_repository() {
    let mut s = base("empty-repository");
    for args in [
        &["status", "--json"][..],
        &["branch", "--json"],
        &["tag", "--json"],
        &["log", "--json"],
        &["diff", "--json"],
        &["diff", "--cached", "--json"],
        &["show", "--json"],
        &["op-log", "--json"],
        &["op-log", "--verify", "--json"],
        &["remote", "list", "--json"],
        &["workspace", "list", "--json"],
        &["identity", "list", "--json"],
        &["operation", "pending", "--json"],
        &["request", "pending", "--json"],
        &["task", "list", "--json"],
        &["meta", "show", "--json"],
        &["verify", "--json"],
        &["undo", "--json"],
        &["add", ".", "--json"],
        &["commit", "-m", "nothing", "--json"],
        &["ci", "validate", "--json"],
    ] {
        s.json(args);
    }
    s.finish();
}

#[test]
fn commits_branches_and_tags() {
    let mut s = base("commits-branches-tags");
    s.write("a.txt", "one\ntwo\nthree\n");
    s.write("dir/b.txt", "b\n");
    s.write("日本語 file.txt", "名前\n");
    s.json(&["add", ".", "--json"]);
    s.json(&["commit", "-m", "feat: first", "--json"]);
    s.write("a.txt", "one\n2\nthree\nfour\n");
    s.json(&["add", "a.txt", "--json"]);
    s.json(&[
        "commit",
        "-m",
        "feat: second \"quoted\" \\ back\ttab",
        "--decision",
        "keep a.txt small",
        "--decision",
        "second decision — with dash",
        "--meta",
        "model=test/1",
        "--json",
    ]);
    s.json(&["commit", "--validate", "--json"]);
    s.json(&["commit", "-m", "empty", "--json"]);
    s.json(&["commit", "-m", "allowed empty", "--allow-empty", "--json"]);
    s.json(&["commit", "--amend", "-m", "amended empty", "--json"]);
    s.json(&["status", "--json"]);
    s.json(&["branch", "--json"]);
    s.json(&["switch", "-c", "feat", "--json"]);
    s.json(&["switch", "feat", "--json"]);
    commit(&mut s, "feat.txt", "feature\n", "feat: on feat");
    s.json(&["switch", "main", "--json"]);
    s.json(&["switch", "missing", "--json"]);
    s.setup(&["branch", "side"]);
    s.json(&["branch", "--json"]);
    s.json(&["merge", "feat", "--json"]);
    s.json(&["merge", "feat", "--json"]);
    s.json(&["branch", "-d", "side", "--json"]);
    s.json(&["tag", "v1", "--json"]);
    s.json(&["tag", "-a", "v2", "-m", "release two", "--json"]);
    s.setup(&["tag", "v0", "HEAD~1"]);
    s.json(&["tag", "--json"]);
    s.json(&["tag", "-d", "v0", "--json"]);
    s.json(&["log", "--json"]);
    s.json(&["log", "--json", "-n", "2"]);
    s.json(&["show", "--json"]);
    s.json(&["show", "HEAD~2", "--json"]);
    s.json(&["show", "v2", "--json"]);
    s.json(&["meta", "show", "--json"]);
    s.json(&["meta", "show", "HEAD~2", "--json"]);
    s.setup(&["meta", "set", "HEAD~1", "-m", "feat: second, corrected"]);
    s.json(&["meta", "show", "HEAD~1", "--json"]);
    s.json(&["verify", "--json"]);
    s.json(&["verify", "-n", "2", "--json"]);
    s.json(&["op-log", "--json"]);
    s.json(&["op-log", "-n", "3", "--json"]);
    s.json(&["op-log", "--verify", "--json"]);
    s.json(&["undo", "--json"]);
    s.json(&["undo", "--json"]);
    s.json(&["op-log", "-n", "2", "--json"]);
    s.json(&["status", "--json"]);
    s.finish();
}

#[test]
fn uncommitted_changes() {
    let mut s = base("uncommitted-changes");
    s.write("kept.txt", "kept\n");
    s.write("edited.txt", "a\nb\nc\nd\ne\nf\ng\nh\n");
    s.write("staged.txt", "old\n");
    s.write("gone.txt", "gone\n");
    s.write("staged-gone.txt", "gone\n");
    s.write(
        "code.rs",
        "fn one() -> u32 {\n    1\n}\n\nfn two() -> u32 {\n    2\n}\n",
    );
    s.write("binary.bin", [0u8, 159, 146, 150, 255, 0, 1]);
    s.write(".gitignore", "ignored/\n");
    s.setup(&["add", "."]);
    s.setup(&["commit", "-m", "base"]);
    s.write("edited.txt", "a\nB\nc\nd\ne\nf\ng\nH\ni\n");
    s.write("staged.txt", "new \u{1f600} \u{7}\n");
    s.setup(&["add", "staged.txt"]);
    std::fs::remove_file(s.path("gone.txt")).unwrap();
    std::fs::remove_file(s.path("staged-gone.txt")).unwrap();
    s.setup(&["add", "staged-gone.txt"]);
    s.write("untracked \"q\".txt", "u\n");
    s.write("ignored/x.txt", "i\n");
    s.write("added.txt", "added\n");
    s.setup(&["add", "added.txt"]);
    s.write(
        "code.rs",
        "fn one() -> u32 {\n    1\n}\n\nfn two() -> u32 { 2 }\n\nfn three() -> u32 {\n    3\n}\n",
    );
    s.write("binary.bin", [0u8, 1, 2, 255]);
    {
        use std::os::unix::ffi::OsStrExt;
        let name = std::ffi::OsStr::from_bytes(b"bad-\xff-name.txt");
        std::fs::write(s.path("").join(name), "bytes\n").unwrap();
    }
    s.json(&["status", "--json"]);
    s.json(&["diff", "--json"]);
    s.json(&["diff", "--cached", "--json"]);
    s.json(&["diff", "--staged", "--json"]);
    s.json(&["diff", "--semantic", "--json"]);
    s.json(&["commit", "--validate", "--json"]);
    s.json(&["ci", "validate", "--json"]);
    s.write(
        ".alt/ci/build/workflow.toml",
        "[trigger]\nref_pattern = \"refs/heads/main\"\non = [\"push\"]\n\n[[step]]\nname = \"build\"\nscript = \"scripts/build.sh\"\n",
    );
    s.json(&["ci", "validate", "--json"]);
    s.write(
        ".alt/ci/broken/workflow.toml",
        "[[step]]\nname = \"test\"\nscript = \"t.sh\"\nneeds = [\"does-not-exist\"]\n",
    );
    s.json(&["ci", "validate", "--json"]);
    s.json(&["add", ".", "--json"]);
    {
        use std::os::unix::ffi::OsStrExt;
        let name = std::ffi::OsStr::from_bytes(b"bad-\xff-name.txt");
        std::fs::remove_file(s.path("").join(name)).unwrap();
    }
    s.json(&["add", ".", "--json"]);
    s.json(&["status", "--json"]);
    s.json(&["undo", "--json"]);
    s.json(&["status", "--json"]);
    s.finish();
}

#[test]
fn flow_and_workspaces() {
    let mut s = base("flow-workspaces");
    commit(&mut s, "a.txt", "a\n", "chore: base");
    s.json(&["flow", "feature", "start", "early", "--json"]);
    s.json(&["flow", "init", "--json"]);
    s.json(&["flow", "init", "--json"]);
    s.json(&["flow", "feature", "start", "login", "--json"]);
    commit(&mut s, "login.txt", "login\n", "feat: add login");
    s.json(&["flow", "feature", "finish", "login", "--json"]);
    s.json(&["flow", "release", "start", "1.0", "--json"]);
    commit(&mut s, "ver.txt", "1.0\n", "chore: bump version");
    s.json(&["flow", "release", "finish", "1.0", "--json"]);
    s.json(&["flow", "hotfix", "start", "fix-crash", "--json"]);
    commit(&mut s, "fix.txt", "fix\n", "fix: crash");
    s.json(&["flow", "hotfix", "finish", "fix-crash", "--json"]);
    s.json(&["flow", "feature", "finish", "absent", "--json"]);
    s.json(&["commit", "-m", "direct", "--allow-empty", "--json"]);
    s.json(&["branch", "--json"]);
    s.json(&["tag", "--json"]);
    let side = s.text("../side");
    s.json(&["workspace", "add", "side", &side, "--json"]);
    s.json(&["workspace", "add", "side", &side, "--json"]);
    s.json(&["workspace", "list", "--json"]);
    s.json(&["--workspace", "side", "status", "--json"]);
    s.json(&["--workspace", "side", "switch", "develop", "--json"]);
    s.cd("side");
    s.json(&["status", "--json"]);
    s.json(&["flow", "feature", "start", "inside", "--json"]);
    s.write("side.txt", "side\n");
    s.json(&["add", ".", "--json"]);
    s.json(&["commit", "-m", "feat: in the workspace", "--json"]);
    s.json(&["op-log", "-n", "2", "--json"]);
    s.cd("repo");
    s.json(&["workspace", "list", "--json"]);
    s.json(&["workspace", "remove", "side", "--json"]);
    s.json(&["workspace", "remove", "side", "--json"]);
    s.json(&["workspace", "list", "--json"]);
    s.finish();
}
