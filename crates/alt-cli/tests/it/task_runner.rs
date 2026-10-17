use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
};
fn run(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_alt"))
        .current_dir(root)
        .args(args)
        .env("ALT_NO_DAEMON", "1")
        .env("USER", "runner-test")
        .env("GIT_AUTHOR_NAME", "runner-test")
        .env("GIT_AUTHOR_EMAIL", "runner@test")
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
    serde_json::from_slice(&ok(root, args).stdout).unwrap()
}
struct Fixture {
    _dir: tempfile::TempDir,
    scratch: tempfile::TempDir,
    main: PathBuf,
    workspace: PathBuf,
    candidate: String,
    profile: String,
}
fn fixture(script: &str) -> Fixture {
    std::fs::create_dir_all(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let directory = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    // This must be on a supported disk and outside every repository ancestor.
    let scratch = tempfile::tempdir_in(std::env::var_os("HOME").unwrap()).unwrap();
    let main = directory.path().join("main");
    let workspace = directory.path().join("task");
    std::fs::create_dir(&main).unwrap();
    ok(&main, &["init", "."]);
    std::fs::write(main.join("source.txt"), "base\n").unwrap();
    std::fs::write(main.join("gate.sh"), script).unwrap();
    std::fs::write(main.join(".gitignore"), ".ignored/\nprobe.txt\ntarget/\n").unwrap();
    ok(&main, &["add", "."]);
    ok(&main, &["commit", "-m", "base"]);
    ok(
        &main,
        &["workspace", "add", "task", workspace.to_str().unwrap()],
    );
    std::fs::create_dir(workspace.join(".ignored")).unwrap();
    std::fs::write(workspace.join(".ignored/probe"), "private probe\n").unwrap();
    std::fs::write(workspace.join("probe.txt"), "root probe\n").unwrap();
    let bindings = json!([{"name":"bash","path":"/bin/bash","version_args":["--version"]}]);
    let environment = json!({"PATH":"/usr/bin:/bin"});
    let observed = value(
        scratch.path(),
        &[
            "runner",
            "probe",
            "--bindings",
            &bindings.to_string(),
            "--environment",
            &environment.to_string(),
            "--json",
        ],
    );
    let definition=json!({"purpose":"validate isolated snapshot","authority":"test-owner","allowed_paths":["source.txt"],"auxiliary":[{"path":".ignored","purpose":"private probe"},{"path":"probe.txt","purpose":"root probe"}],"exclusions":[],"validation":{"commands":[["bash","gate.sh"]],"cwd":".","environment":{"values":environment,"complete":true},"tools":observed["tools"],"external":[],"validation":{"outputs":["target"]}}}).to_string();
    let task = value(
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
    let candidate = value(
        &workspace,
        &[
            "candidate",
            "create",
            task["task_id"].as_str().unwrap(),
            "--json",
        ],
    );
    let profile =
        json!({"kind":"local","scratch":scratch.path(),"tools":bindings,"timeout_ms":2000})
            .to_string();
    Fixture {
        _dir: directory,
        scratch,
        main,
        workspace,
        candidate: candidate["candidate_id"].as_str().unwrap().into(),
        profile,
    }
}
fn result(f: &Fixture, request: &str) -> Output {
    run(
        &f.workspace,
        &[
            "--request-id",
            request,
            "evidence",
            "run",
            &f.candidate,
            "--profile",
            &f.profile,
            "--json",
        ],
    )
}
fn failure(output: &Output) -> Value {
    assert!(!output.status.success());
    serde_json::from_slice::<Value>(&output.stderr).unwrap()["error"].clone()
}
#[test]
fn isolated_run_replay_and_read_only_collection_preserve_exact_logs() {
    let f = fixture(
        "test \"$(cat source.txt)\" = base || exit 9\ntest -f .ignored/probe && test -f probe.txt || exit 8\nprintf 'before\\377\\000after\\n'\nprintf 'diagnostic\\n' >&2\nprintf output > target/result\n",
    );
    std::fs::write(f.workspace.join("source.txt"), "unstaged user edit\n").unwrap();
    let request = "d0500000000000000000000000000001";
    let first = result(&f, request);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let record: Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(record["validity"]["eligible"], true);
    assert_eq!(record["evidence"]["provenance"], "observed");
    let repeat = result(&f, request);
    assert_eq!(repeat.stdout, first.stdout);
    assert_eq!(repeat.status.code(), Some(0));
    assert_eq!(
        std::fs::read_dir(f.scratch.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_str().unwrap().starts_with("alt-run-"))
            .count(),
        1
    );
    let evidence = record["evidence_id"].as_str().unwrap();
    assert_eq!(
        ok(&f.workspace, &["evidence", "log", evidence, "stdout"]).stdout,
        b"before\xff\0after\n"
    );
    assert_eq!(
        ok(&f.workspace, &["evidence", "log", evidence, "stderr"]).stdout,
        b"diagnostic\n"
    );
    assert_eq!(
        std::fs::read(f.workspace.join("source.txt")).unwrap(),
        b"unstaged user edit\n"
    );
    assert_eq!(std::fs::read(f.main.join("source.txt")).unwrap(), b"base\n");
    let attempt = record["attempt_id"].as_str().unwrap();
    let status = value(&f.workspace, &["evidence", "run-status", attempt, "--json"]);
    assert_eq!(status["receipts"][0]["collection_complete"], true);
    // Remove only the response receipt to simulate controller completion loss.
    std::fs::remove_file(
        f.main
            .join(".alt/requests")
            .join(request)
            .join("completed.json"),
    )
    .unwrap();
    assert_eq!(
        value(&f.workspace, &["evidence", "show", evidence, "--json"])["creation_state"],
        "unknown"
    );
    let blocked = result(&f, "d0500000000000000000000000000002");
    assert_eq!(blocked.status.code(), Some(75));
    assert_eq!(failure(&blocked)["details"]["attempt_id"], attempt);
    let collected = value(&f.workspace, &["evidence", "collect", attempt, "--json"]);
    assert_eq!(collected["validity"]["eligible"], true);
    assert_eq!(
        collected["evidence"]["started_unix_ns"],
        record["evidence"]["started_unix_ns"]
    );
    assert_eq!(
        collected["evidence"]["stdout"],
        record["evidence"]["stdout"]
    );
}
#[test]
fn source_writeback_new_inputs_and_failed_commands_are_retained() {
    for script in [
        "printf altered > source.txt\nprintf preserved\n",
        "printf added > new-source.rs\nprintf preserved\n",
        "printf preserved\nprintf error >&2\nexit 9\n",
    ] {
        let f = fixture(script);
        let output = result(&f, "d0500000000000000000000000000010");
        assert_eq!(
            output.status.code(),
            Some(1),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let error = failure(&output);
        assert_eq!(error["outcome"], "applied");
        let record = &error["details"];
        assert_eq!(record["validity"]["eligible"], false);
        let evidence = record["evidence_id"].as_str().unwrap();
        assert_eq!(
            value(&f.workspace, &["evidence", "show", evidence, "--json"])["creation_state"],
            "completed"
        );
        assert_eq!(
            ok(&f.workspace, &["evidence", "log", evidence, "stdout"]).stdout,
            b"preserved"
        );
        assert_eq!(
            std::fs::read(f.workspace.join("source.txt")).unwrap(),
            b"base\n"
        );
        assert!(!f.workspace.join("new-source.rs").exists());
    }
}
#[test]
fn modified_report_cannot_be_promoted_by_collect() {
    let f = fixture("printf failed\nexit 9\n");
    let output = result(&f, "d0500000000000000000000000000020");
    let record = failure(&output)["details"].clone();
    let attempt = record["attempt_id"].as_str().unwrap();
    let status = value(&f.workspace, &["evidence", "run-status", attempt, "--json"]);
    let report = Path::new(status["attempt"]["local_directory"].as_str().unwrap())
        .join(".alt-runner/report.json");
    let mut edited: Value = serde_json::from_slice(&std::fs::read(&report).unwrap()).unwrap();
    edited["evidence"]["termination"] = json!({"kind":"exit","code":0});
    std::fs::write(&report, serde_json::to_vec(&edited).unwrap()).unwrap();
    let rejected = run(&f.workspace, &["evidence", "collect", attempt, "--json"]);
    assert_eq!(rejected.status.code(), Some(1));
    assert!(
        failure(&rejected)["message"]
            .as_str()
            .unwrap()
            .contains("authentication")
    );
    let previous = value(
        &f.workspace,
        &[
            "evidence",
            "show",
            record["evidence_id"].as_str().unwrap(),
            "--json",
        ],
    );
    assert_eq!(previous["evidence"]["termination"]["code"], 9);
}
#[test]
fn stale_auxiliary_or_target_refuses_launch_without_a_run_directory() {
    for move_target in [false, true] {
        let f = fixture("printf should-not-run\n");
        if move_target {
            std::fs::write(f.main.join("source.txt"), "target moved\n").unwrap();
            ok(&f.main, &["add", "source.txt"]);
            ok(&f.main, &["commit", "-m", "advance"]);
        } else {
            std::fs::write(f.workspace.join(".ignored/probe"), "changed\n").unwrap();
        }
        assert_eq!(
            result(&f, "d0500000000000000000000000000030").status.code(),
            Some(1)
        );
        assert_eq!(std::fs::read_dir(f.scratch.path()).unwrap().count(), 0);
    }
}

#[test]
fn equal_request_ids_in_different_repositories_have_distinct_run_directories() {
    let first = fixture("printf first\n");
    let mut second = fixture("printf second\n");
    let mut profile: Value = serde_json::from_str(&second.profile).unwrap();
    profile["scratch"] = json!(first.scratch.path());
    second.profile = profile.to_string();
    let request = "d0500000000000000000000000000040";
    let mut attempts = Vec::new();
    for f in [&first, &second] {
        let output = result(f, request);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let record: Value = serde_json::from_slice(&output.stdout).unwrap();
        let status = value(
            &f.workspace,
            &[
                "evidence",
                "run-status",
                record["attempt_id"].as_str().unwrap(),
                "--json",
            ],
        );
        assert_eq!(status["attempt"]["creator_request"], request);
        attempts.push(status["attempt"].clone());
    }
    assert_ne!(attempts[0]["nonce"], attempts[1]["nonce"]);
    assert_ne!(
        attempts[0]["local_directory"],
        attempts[1]["local_directory"]
    );
}
