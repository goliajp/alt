use super::Scene;

#[test]
fn outside_a_repository() {
    let mut s = Scene::new("outside-a-repository");
    s.cd("plain");
    for args in [
        &["status", "--json"][..],
        &["add", ".", "--json"],
        &["commit", "-m", "x", "--json"],
        &["branch", "--json"],
        &["switch", "main", "--json"],
        &["show", "--json"],
        &["diff", "--json"],
        &["merge", "x", "--json"],
        &["rebase", "x", "--json"],
        &["revert", "x", "--json"],
        &["cherry-pick", "x", "--json"],
        &["tag", "--json"],
        &["log", "--json"],
        &["flow", "init", "--json"],
        &["undo", "--json"],
        &["operation", "pending", "--json"],
        &["workspace", "list", "--json"],
        &["remote", "list", "--json"],
        &["fetch", "--json"],
        &["push", "--json"],
        &["identity", "list", "--json"],
        &["op-log", "--json"],
        &["ci", "validate", "--json"],
        &["verify", "--json"],
        &["meta", "show", "--json"],
        &["request", "pending", "--json"],
        &[
            "request",
            "status",
            "0123456789abcdef0123456789abcdef",
            "--json",
        ],
        &["task", "list", "--json"],
        &["candidate", "show", "x", "--json"],
        &["evidence", "show", "x", "--json"],
        &[
            "--request-id",
            "0123456789abcdef0123456789abcdef",
            "add",
            ".",
            "--json",
        ],
    ] {
        s.json(args);
    }
    s.finish();
}

#[test]
fn usage_errors() {
    let mut s = Scene::new("usage-errors");
    s.cd("repo");
    s.setup(&["init", "."]);
    s.write("a.txt", "a\n");
    s.setup(&["add", "."]);
    s.setup(&["commit", "-m", "base"]);
    for args in [
        &["switch", "--does-not-exist", "--json"][..],
        &["--request-id", "bad", "add", ".", "--json"],
        &["commit", "--json"],
        &["commit", "--no-edit", "-m", "x", "--json"],
        &["rebase", "--json"],
        &["rebase", "main", "--abort", "--json"],
        &["tag", "-a", "v1", "--json"],
        &["meta", "set", "--meta", "novalue", "--json"],
        &["nonesuch", "--json"],
        &["--json"],
        &[
            "task",
            "create",
            "--definition",
            "{",
            "--target",
            "refs/heads/main",
            "--json",
        ],
        &[
            "task",
            "create",
            "--definition",
            "{\"purpose\":1}",
            "--target",
            "refs/heads/main",
            "--json",
        ],
        &[
            "runner",
            "probe",
            "--bindings",
            "[{\"name\":1}]",
            "--environment",
            "{}",
            "--json",
        ],
        &[
            "runner",
            "probe",
            "--bindings",
            "[]",
            "--environment",
            "[1]",
            "--json",
        ],
        &["evidence", "log", "x", "nostream", "--json"],
        &["show", "absent", "--json"],
        &["show", "不存在-日本語", "--json"],
        &["switch", "不存在-日本語", "--json"],
        &["merge", "absent", "--json"],
        &["cherry-pick", "absent", "--json"],
        &["branch", "-d", "absent", "--json"],
        &["branch", "-d", "main", "--json"],
        &["tag", "-d", "absent", "--json"],
        &["log", "--json", "absent"],
        &["operation", "status", "absent", "--json"],
        &["operation", "recover", "1", "--json"],
        &["request", "status", "bad", "--json"],
        &[
            "request",
            "result",
            "0123456789abcdef0123456789abcdef",
            "--json",
        ],
        &["workspace", "remove", "absent", "--json"],
        &["remote", "remove", "absent", "--json"],
        &["fetch", "absent", "--json"],
        &["push", "absent", "--json"],
        &["identity", "trust", "bob", "absent.pub", "--json"],
        &["task", "show", "absent", "--json"],
        &["task", "abandon", "absent", "--reason", "r", "--json"],
        &["candidate", "create", "absent", "--json"],
        &["evidence", "check", "absent", "--json"],
        &[
            "auth",
            "status",
            "--credential-file",
            "absent.json",
            "--json",
        ],
        &[
            "auth",
            "logout",
            "--credential-file",
            "absent.json",
            "--json",
        ],
        &["portable", "export-status", "--json"],
    ] {
        s.json(args);
    }
    s.finish();
}
