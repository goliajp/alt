use super::Scene;

const IDS: [&str; 6] = [
    "000102030405060708090a0b0c0d0e0f",
    "101112131415161718191a1b1c1d1e1f",
    "202122232425262728292a2b2c2d2e2f",
    "303132333435363738393a3b3c3d3e3f",
    "404142434445464748494a4b4c4d4e4f",
    "505152535455565758595a5b5c5d5e5f",
];

/// A request id makes the command result pass through the generic JSON
/// value once more, and stores a receipt that later commands read back.
#[test]
fn request_receipts() {
    let mut s = Scene::new("request-receipts");
    s.volatile(&["started_ms", "completed_ms"]);
    s.cd("repo");
    s.setup(&["init", "."]);
    s.write("a.txt", "one\n");
    s.write("名前.txt", "two\n");
    s.json(&["--request-id", IDS[0], "add", ".", "--json"]);
    s.json(&["--request-id", IDS[0], "add", ".", "--json"]);
    s.json(&["--request-id", IDS[0], "commit", "-m", "other", "--json"]);
    s.json(&[
        "--request-id",
        IDS[1],
        "commit",
        "-m",
        "first",
        "--decision",
        "d",
        "--json",
    ]);
    s.json(&[
        "--request-id",
        IDS[1],
        "commit",
        "-m",
        "first",
        "--decision",
        "d",
        "--json",
    ]);
    s.json(&["--request-id", IDS[2], "switch", "-c", "feat", "--json"]);
    s.write("a.txt", "feat\n");
    s.setup(&["add", "."]);
    s.setup(&["commit", "-m", "feat"]);
    s.setup(&["switch", "main"]);
    s.write("a.txt", "main\n");
    s.setup(&["add", "."]);
    s.setup(&["commit", "-m", "main"]);
    s.json(&["--request-id", IDS[3], "merge", "feat", "--json"]);
    s.json(&["--request-id", IDS[3], "merge", "feat", "--json"]);
    s.json(&["--request-id", IDS[4], "switch", "不存在-日本語", "--json"]);
    s.json(&["--request-id", IDS[5], "status"]);
    s.json(&["request", "pending", "--json"]);
    for id in IDS {
        s.json(&["request", "status", id, "--json"]);
        s.json(&["request", "result", id, "--json"]);
    }
    s.json(&["request", "status", IDS[1]]);
    s.json(&["request", "result", IDS[3]]);
    s.json(&["op-log", "--json"]);
    s.json(&["operation", "pending", "--json"]);
    s.finish();
}
