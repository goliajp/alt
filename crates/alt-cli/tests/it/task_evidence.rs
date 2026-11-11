use alt_json::{Value, json};
use std::{
    path::Path,
    process::{Command, Output},
};

fn run(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_alt"))
        .current_dir(root)
        .args(args)
        .env("ALT_NO_DAEMON", "1")
        .env("USER", "task-test")
        .env("GIT_AUTHOR_NAME", "task-test")
        .env("GIT_AUTHOR_EMAIL", "task@test")
        .env_remove("ALT_PROTECTED_OVERRIDE")
        .output()
        .unwrap()
}
fn ok(root: &Path, args: &[&str]) -> Output {
    let output = run(root, args);
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}
fn value(root: &Path, args: &[&str]) -> Value {
    alt_json::from_slice(&ok(root, args).stdout).unwrap()
}
struct Fixture {
    _dir: alt_os::temp::TempDir,
    main: std::path::PathBuf,
    workspace: std::path::PathBuf,
    task: String,
}
fn fixture() -> Fixture {
    // Task execution requires a supported disk filesystem; runner /tmp may
    // be tmpfs. Cargo's test scratch directory shares the build filesystem.
    std::fs::create_dir_all(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let directory = alt_os::temp::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let main = directory.path().join("main");
    let workspace = directory.path().join("task");
    std::fs::create_dir(&main).unwrap();
    ok(&main, &["init", "."]);
    std::fs::write(main.join("source.txt"), "base\n").unwrap();
    std::fs::write(main.join("outside.txt"), "outside authority\n").unwrap();
    std::fs::write(main.join(".gitignore"), ".ignored/\n").unwrap();
    ok(&main, &["add", "."]);
    ok(&main, &["commit", "-m", "base"]);
    ok(
        &main,
        &["workspace", "add", "task", workspace.to_str().unwrap()],
    );
    std::fs::create_dir(workspace.join(".ignored")).unwrap();
    for path in ["probe", "generator", "fixture"] {
        std::fs::write(workspace.join(".ignored").join(path), "base\n").unwrap();
    }
    let definition = json!({"purpose":"repair source","authority":"test-owner","allowed_paths":["source.txt"],"auxiliary":[{"path":".ignored","purpose":"validation inputs"}],"exclusions":[],"validation":{"commands":[["sh","gate"]],"cwd":".","environment":{"values":{},"complete":true},"tools":[],"external":[],"validation":{"required":"gate"}}}).to_string();
    let result = value(
        &workspace,
        &[
            "task",
            "create",
            "--definition",
            &definition,
            "--target",
            "refs/heads/main",
            "--json",
        ],
    );
    Fixture {
        _dir: directory,
        main,
        workspace,
        task: result["task_id"].as_str().unwrap().into(),
    }
}
fn candidate(f: &Fixture) -> Value {
    value(&f.workspace, &["candidate", "create", &f.task, "--json"])
}

#[test]
fn source_probe_generator_fixture_and_new_member_invalidate_candidates() {
    let f = fixture();
    let first = candidate(&f);
    let id = first["candidate_id"].as_str().unwrap();
    for path in [".ignored/probe", ".ignored/generator", ".ignored/fixture"] {
        std::fs::write(f.workspace.join(path), "changed\n").unwrap();
        assert_eq!(
            value(&f.workspace, &["candidate", "check", id, "--json"])["current"],
            false
        );
        let next = candidate(&f);
        assert_ne!(next["candidate_id"], first["candidate_id"]);
        std::fs::write(f.workspace.join(path), "base\n").unwrap();
    }
    std::fs::write(f.workspace.join(".ignored/new"), "new\n").unwrap();
    assert_eq!(
        value(&f.workspace, &["candidate", "check", id, "--json"])["current"],
        false
    );
    std::fs::remove_file(f.workspace.join(".ignored/new")).unwrap();
    std::fs::write(f.workspace.join("source.txt"), "unstaged\n").unwrap();
    assert_eq!(
        value(&f.workspace, &["candidate", "check", id, "--json"])["current"],
        true,
        "explicit index selection excludes unstaged edits"
    );
    ok(&f.workspace, &["add", "source.txt"]);
    assert_eq!(
        value(&f.workspace, &["candidate", "check", id, "--json"])["current"],
        false
    );
    assert_eq!(std::fs::read(f.main.join("source.txt")).unwrap(), b"base\n");
    assert_eq!(
        value(&f.workspace, &["candidate", "show", id, "--json"])["candidate"],
        first["candidate"]
    );
}
#[test]
fn authority_workspace_and_unknown_creation_prevent_new_candidates() {
    let f = fixture();
    std::fs::write(f.workspace.join("outside.txt"), "not authorized\n").unwrap();
    ok(&f.workspace, &["add", "outside.txt"]);
    let output = run(&f.workspace, &["candidate", "create", &f.task, "--json"]);
    assert_eq!(output.status.code(), Some(1));
    let output = run(&f.main, &["candidate", "create", &f.task, "--json"]);
    assert_eq!(output.status.code(), Some(1));
    let task = value(&f.workspace, &["task", "show", &f.task, "--json"]);
    let request = task["task"]["creator_request"].as_str().unwrap();
    std::fs::remove_file(
        f.main
            .join(".alt/requests")
            .join(request)
            .join("completed.json"),
    )
    .unwrap();
    assert_eq!(
        value(&f.workspace, &["task", "show", &f.task, "--json"])["creation_state"],
        "unknown"
    );
    assert_eq!(
        run(&f.workspace, &["candidate", "create", &f.task, "--json"])
            .status
            .code(),
        Some(75)
    );
}
#[test]
fn abandonment_retains_candidates_and_working_files() {
    let f = fixture();
    let first = candidate(&f);
    ok(
        &f.workspace,
        &[
            "task",
            "abandon",
            &f.task,
            "--reason",
            "different approach",
            "--json",
        ],
    );
    assert_eq!(
        value(
            &f.workspace,
            &[
                "candidate",
                "show",
                first["candidate_id"].as_str().unwrap(),
                "--json"
            ]
        )["candidate"],
        first["candidate"]
    );
    assert_eq!(
        std::fs::read(f.workspace.join("source.txt")).unwrap(),
        b"base\n"
    );
    assert_eq!(
        run(&f.workspace, &["candidate", "create", &f.task, "--json"])
            .status
            .code(),
        Some(1)
    );
}

#[test]
fn candidate_retries_preserve_identity_and_operation_attribution() {
    let f = fixture();
    let request = "d0400000000000000000000000000001";
    let args = [
        "--request-id",
        request,
        "candidate",
        "create",
        &f.task,
        "--json",
    ];
    let first = ok(&f.workspace, &args);
    let repeated = ok(&f.workspace, &args);
    assert_eq!(first.stdout, repeated.stdout);
    let created: Value = alt_json::from_slice(&first.stdout).unwrap();
    let operation = value(
        &f.workspace,
        &[
            "operation",
            "status",
            created["operation_id"].as_str().unwrap(),
            "--json",
        ],
    );
    assert_eq!(operation["operation"]["task_id"], f.task);
    assert_eq!(operation["operation"]["record_id"], created["candidate_id"]);
    assert_eq!(operation["operation"]["request_id"], request);
    assert_eq!(operation["operation"]["action"], "candidate-create");
    let task = value(&f.workspace, &["task", "show", &f.task, "--json"]);
    assert_eq!(
        operation["operation"]["workspace"]["instance"],
        task["task"]["scope"]["workspace"]
    );
}

#[test]
fn advancing_target_invalidates_candidate_without_changing_its_inputs() {
    let f = fixture();
    let first = candidate(&f);
    let id = first["candidate_id"].as_str().unwrap();
    std::fs::write(f.main.join("source.txt"), "target moved\n").unwrap();
    ok(&f.main, &["add", "source.txt"]);
    ok(&f.main, &["commit", "-m", "advance target"]);
    let checked = value(&f.workspace, &["candidate", "check", id, "--json"]);
    assert_eq!(checked["current"], false);
    assert_eq!(
        checked["current_manifest_id"],
        checked["recorded_manifest_id"]
    );
    assert_ne!(
        checked["current_target"],
        first["candidate"]["target_expected"]
    );
    assert_eq!(
        std::fs::read(f.workspace.join("source.txt")).unwrap(),
        b"base\n"
    );
}

#[test]
fn external_success_claims_and_failures_keep_raw_bytes_without_becoming_observed() {
    let f = fixture();
    let candidate = candidate(&f);
    let candidate_id = candidate["candidate_id"].as_str().unwrap();
    let manifest_id = candidate["candidate"]["manifest"].as_str().unwrap();
    let full = value(&f.workspace, &["candidate", "show", candidate_id, "--json"]);
    let raw = b"private-task-log-329ae1d4\xff\0\na".to_vec();
    let stdout = f._dir.path().join("output.bin");
    std::fs::write(&stdout, &raw).unwrap();
    let report = json!({"schema_version":1,"candidate":candidate_id,"manifest":manifest_id,"before":manifest_id,"after":manifest_id,"runner":"external","actual_context":full["input_manifest"]["context"],"binaries":{},"started_unix_ns":1,"finished_unix_ns":2,"termination":{"kind":"exit","code":0},"stdout":null,"stderr":null,"artifacts":{},"provenance":"observed","original_report":null,"creator_request":"external-claim"}).to_string();
    let imported = value(
        &f.workspace,
        &[
            "evidence",
            "import",
            candidate_id,
            "--report",
            &report,
            "--stdout",
            stdout.to_str().unwrap(),
            "--json",
        ],
    );
    let id = imported["evidence_id"].as_str().unwrap();
    assert_eq!(imported["evidence"]["provenance"], "reported");
    assert_eq!(imported["eligible"], false);
    assert_eq!(
        ok(&f.workspace, &["evidence", "log", id, "stdout"]).stdout,
        raw
    );
    assert_eq!(
        ok(&f.workspace, &["evidence", "log", id, "original-report"]).stdout,
        report.as_bytes()
    );
    let checked = value(&f.workspace, &["evidence", "check", id, "--json"]);
    assert_eq!(checked["validity"]["eligible"], false);
    assert!(
        checked["validity"]["reasons"]
            .as_array()
            .unwrap()
            .contains(&json!("stderr_missing"))
    );
    let mut failed: Value = alt_json::from_str(&report).unwrap();
    failed["termination"]["code"] = json!(9);
    let failed = value(
        &f.workspace,
        &[
            "evidence",
            "import",
            candidate_id,
            "--report",
            &failed.to_string(),
            "--json",
        ],
    );
    assert_ne!(failed["evidence_id"], imported["evidence_id"]);
    assert_eq!(
        value(&f.workspace, &["evidence", "show", id, "--json"])["evidence"],
        imported["evidence"]
    );
    let exported = f._dir.path().join("exported");
    ok(&f.main, &["export", exported.to_str().unwrap()]);
    let objects = Command::new("git")
        .arg("-C")
        .arg(&exported)
        .args(["cat-file", "--batch-all-objects", "--batch"])
        .output()
        .unwrap();
    assert!(objects.status.success());
    assert!(
        !objects
            .stdout
            .windows(report.len())
            .any(|window| window == report.as_bytes()),
        "private report entered the Git ODB"
    );
    assert!(
        !objects
            .stdout
            .windows(raw.len())
            .any(|window| window == raw),
        "private raw log entered the Git ODB"
    );
    assert!(!exported.join(".alt").exists());
}
