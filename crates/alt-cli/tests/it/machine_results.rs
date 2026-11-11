//! Machine results and whole-command receipts are identical across transports.
use alt_json::Value;
use std::path::Path;
use std::process::{Command, Output};

const ID: &str = "0123456789abcdef0123456789abcdef";

fn run(root: &Path, args: &[&str], daemon: bool) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_alt"));
    command
        .current_dir(root)
        .args(args)
        .env("USER", "machine-test")
        .env("GIT_AUTHOR_NAME", "machine-test")
        .env("GIT_AUTHOR_EMAIL", "machine@test")
        .env("ALT_DAEMON_IDLE_MS", "300")
        .env_remove("ALT_PROTECTED_OVERRIDE");
    if daemon {
        command.env_remove("ALT_NO_DAEMON");
    } else {
        command.env("ALT_NO_DAEMON", "1");
    }
    command.output().unwrap()
}
fn ok(root: &Path, args: &[&str]) -> Output {
    let output = run(root, args, false);
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}
fn fixture() -> alt_os::temp::TempDir {
    let directory = alt_os::temp::tempdir().unwrap();
    ok(directory.path(), &["init", "."]);
    std::fs::write(directory.path().join("a.txt"), "base\n").unwrap();
    ok(directory.path(), &["add", "."]);
    ok(directory.path(), &["commit", "-m", "base"]);
    directory
}
fn error(output: &Output, code: i32, kind: &str) -> Value {
    assert_eq!(
        output.status.code(),
        Some(code),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty(), "failure leaked stdout");
    let value: Value = alt_json::from_slice(&output.stderr).unwrap();
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["error"]["kind"], kind);
    for field in [
        "message",
        "outcome",
        "retryable",
        "request_id",
        "operation_id",
        "details",
        "next_action",
    ] {
        assert!(value["error"].get(field).is_some(), "missing {field}");
    }
    value
}
fn same(first: &Output, second: &Output) {
    assert_eq!(first.status.code(), second.status.code());
    assert_eq!(first.stdout, second.stdout);
    assert_eq!(first.stderr, second.stderr);
}

#[test]
fn ordinary_refusal_is_unicode_json_and_replays_across_transports() {
    for daemon in [false, true] {
        let root = fixture();
        let args = ["--request-id", ID, "switch", "不存在-日本語", "--json"];
        let first = run(root.path(), &args, daemon);
        let value = error(&first, 1, "precondition_failed");
        assert_eq!(value["error"]["outcome"], "not_applied");
        assert_eq!(value["error"]["request_id"], ID);
        assert!(
            value["error"]["message"]
                .as_str()
                .unwrap()
                .contains("不存在-日本語")
        );
        same(&first, &run(root.path(), &args, !daemon));
        let result = ok(root.path(), &["request", "result", ID, "--json"]);
        let value: Value = alt_json::from_slice(&result.stdout).unwrap();
        assert_eq!(
            value["request"]["stderr"]["text"],
            String::from_utf8(first.stderr).unwrap()
        );
        assert_eq!(value["request"]["exit_code"], 1);
    }
}

#[test]
fn usage_errors_have_a_single_machine_result() {
    let root = fixture();
    let value = error(
        &run(
            root.path(),
            &["switch", "--does-not-exist", "--json"],
            false,
        ),
        2,
        "usage",
    );
    assert_eq!(value["error"]["outcome"], "not_applied");
    error(
        &run(
            root.path(),
            &["--request-id", "bad", "add", ".", "--json"],
            false,
        ),
        2,
        "usage",
    );
}

#[test]
fn completed_add_replays_before_reading_changed_worktree() {
    let root = fixture();
    std::fs::write(root.path().join("a.txt"), "first\n").unwrap();
    let args = ["--request-id", ID, "add", "a.txt", "--json"];
    let first = run(root.path(), &args, true);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let index = std::fs::read(root.path().join(".alt/index")).unwrap();
    std::fs::write(root.path().join("a.txt"), "second\n").unwrap();
    same(&first, &run(root.path(), &args, false));
    assert_eq!(
        index,
        std::fs::read(root.path().join(".alt/index")).unwrap()
    );
    let value: Value = alt_json::from_slice(&first.stdout).unwrap();
    assert_eq!(value["request_id"], ID);
    assert!(value["operation_ids"].as_array().unwrap().len() >= 2);
    error(
        &run(
            root.path(),
            &["--request-id", ID, "add", ".", "--json"],
            true,
        ),
        1,
        "request_mismatch",
    );
}

#[test]
fn pending_and_corrupt_receipts_never_reexecute() {
    let root = fixture();
    let args = ["--request-id", ID, "switch", "main", "--json"];
    let first = run(root.path(), &args, false);
    assert!(first.status.success());
    let directory = root.path().join(".alt/requests").join(ID);
    let completed = std::fs::read(directory.join("completed.json")).unwrap();
    std::fs::remove_file(directory.join("completed.json")).unwrap();
    error(&run(root.path(), &args, true), 75, "result_unknown");
    let pending = ok(root.path(), &["request", "pending", "--json"]);
    let pending: Value = alt_json::from_slice(&pending.stdout).unwrap();
    assert!(
        pending["requests"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["request_id"] == ID)
    );
    std::fs::write(directory.join("completed.json"), completed).unwrap();
    std::fs::write(directory.join("response.bin"), "torn").unwrap();
    error(&run(root.path(), &args, false), 75, "result_unknown");
}

#[test]
fn request_binding_rejects_another_workspace_or_principal() {
    let root = fixture();
    let workspace = alt_os::temp::tempdir().unwrap();
    ok(root.path(), &["branch", "other"]);
    ok(
        root.path(),
        &[
            "workspace",
            "add",
            "other",
            workspace.path().to_str().unwrap(),
            "other",
        ],
    );
    let args = ["--request-id", ID, "add", ".", "--json"];
    assert!(run(root.path(), &args, false).status.success());
    error(&run(workspace.path(), &args, false), 1, "request_mismatch");
    let output = Command::new(env!("CARGO_BIN_EXE_alt"))
        .current_dir(root.path())
        .args(args)
        .env("ALT_NO_DAEMON", "1")
        .env("USER", "another-principal")
        .env("GIT_AUTHOR_NAME", "machine-test")
        .env("GIT_AUTHOR_EMAIL", "machine@test")
        .output()
        .unwrap();
    error(&output, 1, "request_mismatch");
}

#[cfg(unix)]
#[test]
fn non_utf8_argument_has_exact_bytes_without_creating_a_request() {
    use std::os::unix::ffi::OsStringExt;
    let root = fixture();
    let output = Command::new(env!("CARGO_BIN_EXE_alt"))
        .current_dir(root.path())
        .arg("add")
        .arg(std::ffi::OsString::from_vec(vec![b'a', 0xff]))
        .arg("--json")
        .output()
        .unwrap();
    let value = error(&output, 2, "unsupported_path");
    assert_eq!(value["error"]["details"]["bytes_hex"], "61ff");
}

#[test]
fn capability_denial_is_the_same_category_on_both_transports() {
    for daemon in [false, true] {
        let root = fixture();
        std::fs::write(
            root.path().join(".alt/policy"),
            "human:machine-test -> read-only\n",
        )
        .unwrap();
        let index = std::fs::read(root.path().join(".alt/index")).unwrap();
        let value = error(
            &run(root.path(), &["add", ".", "--json"], daemon),
            1,
            "capability_denied",
        );
        assert_eq!(value["error"]["outcome"], "not_applied");
        assert_eq!(
            index,
            std::fs::read(root.path().join(".alt/index")).unwrap()
        );
    }
}

#[test]
fn merge_conflict_keeps_details_and_replays_without_touching_resolution() {
    let root = fixture();
    ok(root.path(), &["switch", "-c", "other"]);
    std::fs::write(root.path().join("a.txt"), "theirs\n").unwrap();
    ok(root.path(), &["add", "."]);
    ok(root.path(), &["commit", "-m", "theirs"]);
    ok(root.path(), &["switch", "main"]);
    std::fs::write(root.path().join("a.txt"), "ours\n").unwrap();
    ok(root.path(), &["add", "."]);
    ok(root.path(), &["commit", "-m", "ours"]);
    let args = ["--request-id", ID, "merge", "other", "--json"];
    let first = run(root.path(), &args, true);
    let value = error(&first, 1, "conflict");
    assert_eq!(value["error"]["outcome"], "partial");
    assert!(
        value["error"]["details"]["command_output"]["text"]
            .as_str()
            .unwrap()
            .contains("a.txt")
    );
    std::fs::write(root.path().join("a.txt"), "resolution\n").unwrap();
    same(&first, &run(root.path(), &args, false));
    assert_eq!(
        std::fs::read_to_string(root.path().join("a.txt")).unwrap(),
        "resolution\n"
    );
}

#[test]
fn legacy_key_before_ref_snapshot_is_unknown_and_is_not_executed() {
    let root = fixture();
    let key = alt_cli::request::parse_id(ID).unwrap();
    {
        let mut refs = alt_refs::RefStore::open(root.path().join(".alt")).unwrap();
        refs.commit_idempotent("old-client", 1, &[], Some(key), None)
            .unwrap();
        refs.snapshot().unwrap();
    }
    let before = std::fs::read(root.path().join(".alt/index")).unwrap();
    std::fs::write(root.path().join("a.txt"), "must not be staged\n").unwrap();
    let value = error(
        &run(
            root.path(),
            &["--request-id", ID, "add", ".", "--json"],
            false,
        ),
        75,
        "result_unknown",
    );
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("legacy")
    );
    assert_eq!(
        before,
        std::fs::read(root.path().join(".alt/index")).unwrap()
    );
}

#[test]
fn io_failure_during_a_write_is_unknown_on_both_transports() {
    for daemon in [false, true] {
        let root = fixture();
        let index = root.path().join(".alt/index");
        std::fs::remove_file(&index).unwrap();
        std::fs::create_dir(&index).unwrap();
        let value = error(
            &run(
                root.path(),
                &["--request-id", ID, "add", ".", "--json"],
                daemon,
            ),
            75,
            "result_unknown",
        );
        assert_eq!(value["error"]["outcome"], "unknown");
        assert_eq!(value["error"]["request_id"], ID);
        let status = ok(root.path(), &["request", "status", ID, "--json"]);
        let status: Value = alt_json::from_slice(&status.stdout).unwrap();
        assert_eq!(status["request"]["outcome"], "unknown");
    }
}

#[test]
fn legacy_ref_undo_is_a_known_refusal() {
    let root = fixture();
    {
        let mut refs = alt_refs::RefStore::open(root.path().join(".alt")).unwrap();
        refs.commit("legacy", 1, &[]).unwrap();
    }
    let value = error(
        &run(root.path(), &["undo", "--json"], false),
        1,
        "unsupported_record",
    );
    assert_eq!(value["error"]["outcome"], "not_applied");
}

#[test]
fn recovering_a_suboperation_does_not_complete_the_interrupted_request() {
    let root = fixture();
    std::fs::write(root.path().join("a.txt"), "staged\n").unwrap();
    assert!(
        run(
            root.path(),
            &["--request-id", ID, "add", ".", "--json"],
            false
        )
        .status
        .success()
    );
    let log_path = root.path().join(".alt/oplog/log");
    let bytes = std::fs::read(&log_path).unwrap();
    let (mut cursor, mut last) = (5, 5);
    while cursor < bytes.len() {
        last = cursor;
        let length = u32::from_le_bytes(bytes[cursor..cursor + 4].try_into().unwrap()) as usize;
        cursor += 4 + length + 8;
    }
    // Crash fixture: index replacement survived, its operation completion and
    // whole-command receipt did not. Keep the request intent intact.
    std::fs::write(&log_path, &bytes[..last]).unwrap();
    let directory = root.path().join(".alt/requests").join(ID);
    std::fs::remove_file(directory.join("completed.json")).unwrap();
    std::fs::remove_file(directory.join("response.bin")).unwrap();
    let failed = error(
        &run(root.path(), &["add", ".", "--json"], false),
        75,
        "result_unknown",
    );
    let operation = failed["error"]["operation_id"].as_str().unwrap();
    assert_eq!(operation.len(), 64);
    ok(root.path(), &["operation", "recover", operation, "--json"]);
    let operation_status = ok(root.path(), &["operation", "status", operation, "--json"]);
    let operation_status: Value = alt_json::from_slice(&operation_status.stdout).unwrap();
    assert_eq!(operation_status["operation"]["state"], "completed");
    let request_status = ok(root.path(), &["request", "status", ID, "--json"]);
    let request_status: Value = alt_json::from_slice(&request_status.stdout).unwrap();
    assert_eq!(request_status["request"]["state"], "pending");
    assert_eq!(request_status["request"]["outcome"], "unknown");
    error(
        &run(
            root.path(),
            &["--request-id", ID, "add", ".", "--json"],
            false,
        ),
        75,
        "result_unknown",
    );
}
