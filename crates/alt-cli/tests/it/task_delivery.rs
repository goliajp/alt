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
    _dir: alt_os::temp::TempDir,
    scratch: alt_os::temp::TempDir,
    main: PathBuf,
    workspace: PathBuf,
    task: String,
    candidate: String,
    profile: String,
}
fn fixture(script: &str) -> Fixture {
    fixture_with_paths(script, &["source.txt"])
}
fn fixture_with_paths(script: &str, allowed_paths: &[&str]) -> Fixture {
    std::fs::create_dir_all(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let directory = alt_os::temp::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    // This must be on a supported disk and outside every repository ancestor.
    let scratch = alt_os::temp::tempdir_in(std::env::var_os("HOME").unwrap()).unwrap();
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
    let definition=json!({"purpose":"validate isolated snapshot","authority":"test-owner","allowed_paths":allowed_paths,"auxiliary":[{"path":".ignored","purpose":"private probe"},{"path":"probe.txt","purpose":"root probe"}],"exclusions":[],"validation":{"commands":[["bash","gate.sh"]],"cwd":".","environment":{"values":environment,"complete":true},"tools":observed["tools"],"external":[],"validation":{"outputs":["target"]}}}).to_string();
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
    std::fs::write(workspace.join("source.txt"), "reviewed\n").unwrap();
    ok(&workspace, &["add", "source.txt"]);
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
        task: task["task_id"].as_str().unwrap().into(),
        candidate: candidate["candidate_id"].as_str().unwrap().into(),
        profile,
    }
}
fn observed(f: &Fixture) -> Value {
    value(
        &f.workspace,
        &[
            "evidence",
            "run",
            &f.candidate,
            "--profile",
            &f.profile,
            "--json",
        ],
    )
}
fn review(f: &Fixture, evidence: &str) -> Value {
    value(
        &f.workspace,
        &[
            "task",
            "review",
            &f.candidate,
            "--evidence",
            evidence,
            "--reviewer",
            "fixture-owner",
            "--reason",
            "inspected full change",
            "-m",
            "fix: deliver reviewed source",
            "--json",
        ],
    )
}
fn free_target(f: &Fixture) {
    ok(&f.main, &["switch", "-c", "target-hold"]);
}
fn git_path() -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|p| p.join("git"))
        .find(|p| p.is_file())
        .unwrap()
        .canonicalize()
        .unwrap()
}
fn git(root: &Path, args: &[&str]) -> Vec<u8> {
    let output = Command::new(git_path())
        .current_dir(root)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}
#[test]
fn reviewed_integration_exports_only_accepted_content_and_preserves_other_workspaces() {
    let f = fixture("test \"$(cat source.txt)\" = reviewed\nprintf verified\n");
    let evidence = observed(&f);
    assert_eq!(evidence["validity"]["eligible"], true);
    let changes = value(&f.workspace, &["task", "changes", &f.candidate, "--json"]);
    assert_eq!(changes["diff"]["changes"].as_array().unwrap().len(), 1);
    assert_eq!(changes["diff"]["changes"][0]["before"]["utf8"], "base\n");
    assert_eq!(changes["diff"]["changes"][0]["after"]["utf8"], "reviewed\n");
    let reviewed = review(&f, evidence["evidence_id"].as_str().unwrap());
    let review_id = reviewed["review_id"].as_str().unwrap();
    let occupied = run(&f.workspace, &["task", "integrate", review_id, "--json"]);
    assert_eq!(occupied.status.code(), Some(1));
    free_target(&f);
    let main_index = std::fs::read(f.main.join(".alt/index")).unwrap();
    let request = "d0600000000000000000000000000001";
    let args = [
        "--request-id",
        request,
        "task",
        "integrate",
        review_id,
        "--json",
    ];
    let first = ok(&f.workspace, &args);
    assert_eq!(ok(&f.workspace, &args).stdout, first.stdout);
    let integrated: Value = serde_json::from_slice(&first.stdout).unwrap();
    let integration = integrated["integration_id"].as_str().unwrap();
    assert_eq!(
        value(&f.workspace, &["task", "show", &f.task, "--json"])["state"],
        "integrated"
    );
    assert_eq!(
        std::fs::read(f.main.join(".alt/index")).unwrap(),
        main_index
    );
    assert_eq!(std::fs::read(f.main.join("source.txt")).unwrap(), b"base\n");
    // An unrelated later candidate object must not leak through the export.
    std::fs::write(f.workspace.join("source.txt"), "PRIVATE LATER UNREVIEWED\n").unwrap();
    ok(&f.workspace, &["add", "source.txt"]);
    let output = f.scratch.path().join("git-delivery");
    let exported = value(
        &f.workspace,
        &[
            "task",
            "export",
            integration,
            output.to_str().unwrap(),
            "--git",
            git_path().to_str().unwrap(),
            "--json",
        ],
    );
    assert_eq!(
        std::fs::read(output.join("source.txt")).unwrap(),
        b"reviewed\n"
    );
    assert!(!output.join(".alt").exists());
    assert!(!output.join(".ignored").exists());
    assert!(!output.join("probe.txt").exists());
    let refs = String::from_utf8(git(&output, &["show-ref"])).unwrap();
    assert!(refs.contains("refs/heads/main"));
    assert!(refs.contains("refs/notes/alt/meta"));
    assert!(!refs.contains("refs/heads/task"));
    assert!(!refs.contains("target-hold"));
    assert!(!refs.contains("workspaces/"));
    assert!(git(&output, &["status", "--porcelain"]).is_empty());
    let all = git(&output, &["cat-file", "--batch-all-objects", "--batch"]);
    assert!(
        !all.windows(b"PRIVATE LATER UNREVIEWED".len())
            .any(|v| v == b"PRIVATE LATER UNREVIEWED")
    );
    let shown = value(
        &f.workspace,
        &[
            "task",
            "delivery-show",
            exported["delivery_id"].as_str().unwrap(),
            "--json",
        ],
    );
    assert_eq!(shown["report"], exported["report"]);
    assert_eq!(shown["creation_complete"], true);
    let export_request = exported["delivery"]["creator_request"].as_str().unwrap();
    let delivery_id = exported["delivery_id"].as_str().unwrap();
    std::fs::remove_file(
        f.main
            .join(".alt/requests")
            .join(export_request)
            .join("completed.json"),
    )
    .unwrap();
    assert_eq!(
        value(
            &f.workspace,
            &["task", "delivery-show", delivery_id, "--json"]
        )["creation_complete"],
        false
    );
    let collected = value(
        &f.workspace,
        &[
            "task",
            "delivery-collect",
            delivery_id,
            "--git",
            git_path().to_str().unwrap(),
            "--json",
        ],
    );
    assert_eq!(collected["exported_again"], false);
    std::fs::write(output.join("source.txt"), "tampered delivery\n").unwrap();
    assert_eq!(
        run(
            &f.workspace,
            &[
                "task",
                "delivery-collect",
                delivery_id,
                "--git",
                git_path().to_str().unwrap(),
                "--json"
            ]
        )
        .status
        .code(),
        Some(1)
    );
    assert_eq!(
        std::fs::read(output.join("source.txt")).unwrap(),
        b"tampered delivery\n"
    );
}
#[test]
fn changed_candidate_auxiliary_and_target_refuse_old_review_without_integration() {
    for change in ["source", "auxiliary", "target"] {
        let f = fixture("printf verified\n");
        let evidence = observed(&f);
        let reviewed = review(&f, evidence["evidence_id"].as_str().unwrap());
        match change {
            "source" => {
                std::fs::write(f.workspace.join("source.txt"), "new candidate\n").unwrap();
                ok(&f.workspace, &["add", "source.txt"]);
            }
            "auxiliary" => {
                std::fs::write(f.workspace.join("probe.txt"), "changed probe\n").unwrap()
            }
            "target" => {
                std::fs::write(f.main.join("source.txt"), "target advance\n").unwrap();
                ok(&f.main, &["add", "source.txt"]);
                ok(&f.main, &["commit", "-m", "advance target"]);
            }
            _ => unreachable!(),
        }
        free_target(&f);
        let output = run(
            &f.workspace,
            &[
                "task",
                "integrate",
                reviewed["review_id"].as_str().unwrap(),
                "--json",
            ],
        );
        assert_eq!(
            output.status.code(),
            Some(1),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!f.main.join(".alt/task-data/records/integration").exists());
        assert_eq!(
            value(&f.workspace, &["task", "show", &f.task, "--json"])["state"],
            "active"
        );
    }
}
#[test]
fn failed_validation_and_missing_completion_cannot_grant_review_or_delivery() {
    let f = fixture("printf retained\nexit 9\n");
    let failed = run(
        &f.workspace,
        &[
            "evidence",
            "run",
            &f.candidate,
            "--profile",
            &f.profile,
            "--json",
        ],
    );
    let error: Value = serde_json::from_slice(&failed.stderr).unwrap();
    let evidence = error["error"]["details"]["evidence_id"].as_str().unwrap();
    let denied = run(
        &f.workspace,
        &[
            "task",
            "review",
            &f.candidate,
            "--evidence",
            evidence,
            "--reviewer",
            "owner",
            "--reason",
            "cannot pass",
            "-m",
            "fix: rejected",
            "--json",
        ],
    );
    assert_eq!(denied.status.code(), Some(1));
    let f = fixture("printf verified\n");
    let evidence = observed(&f);
    let reviewed = review(&f, evidence["evidence_id"].as_str().unwrap());
    free_target(&f);
    let integrated = value(
        &f.workspace,
        &[
            "task",
            "integrate",
            reviewed["review_id"].as_str().unwrap(),
            "--json",
        ],
    );
    let id = integrated["integration_id"].as_str().unwrap();
    let request = integrated["integration"]["creator_request"]
        .as_str()
        .unwrap();
    std::fs::remove_file(
        f.main
            .join(".alt/requests")
            .join(request)
            .join("completed.json"),
    )
    .unwrap();
    let queried = value(&f.workspace, &["task", "integration-show", id, "--json"]);
    assert_eq!(queried["state"], "unknown");
    assert!(!queried["operation_ids"].as_array().unwrap().is_empty());
    assert_eq!(
        queried["current_target"],
        integrated["integration"]["commit"]
    );
    assert_eq!(
        value(&f.workspace, &["task", "show", &f.task, "--json"])["state"],
        "unknown"
    );
    let target = f.scratch.path().join("refused-delivery");
    assert!(
        !run(
            &f.workspace,
            &["task", "export", id, target.to_str().unwrap(), "--json"]
        )
        .status
        .success()
    );
    assert!(!target.exists());
    let rerun = run(
        &f.workspace,
        &[
            "task",
            "integrate",
            reviewed["review_id"].as_str().unwrap(),
            "--json",
        ],
    );
    assert_eq!(rerun.status.code(), Some(75));
    let collected = value(&f.workspace, &["task", "integration-collect", id, "--json"]);
    assert_eq!(collected["executed_again"], false);
    assert_eq!(
        value(&f.workspace, &["task", "show", &f.task, "--json"])["state"],
        "integrated"
    );
    let exported = value(
        &f.workspace,
        &[
            "task",
            "export",
            id,
            target.to_str().unwrap(),
            "--git",
            git_path().to_str().unwrap(),
            "--json",
        ],
    );
    assert_eq!(exported["report"]["git"]["fsck_exit"], 0);
}

#[test]
fn prepared_but_unapplied_integration_can_be_abandoned_without_moving_refs() {
    let f = fixture("printf verified\n");
    let evidence = observed(&f);
    let reviewed = review(&f, evidence["evidence_id"].as_str().unwrap());
    // Persist the exact immutable intent shape at the pre-CAS crash boundary.
    let candidate = value(&f.workspace, &["candidate", "show", &f.candidate, "--json"]);
    let task = value(&f.workspace, &["task", "show", &f.task, "--json"]);
    let intent = alt_task::Integration {
        schema_version: 1,
        task: f.task.clone(),
        review: reviewed["review_id"].as_str().unwrap().into(),
        commit: task["task"]["baseline"]["head"].as_str().unwrap().into(),
        target: "refs/heads/main".into(),
        before: task["task"]["baseline"]["head"].as_str().unwrap().into(),
        notes_before: None,
        notes_after: task["task"]["baseline"]["head"].as_str().unwrap().into(),
        creator_request: candidate["candidate"]["creator_request"]
            .as_str()
            .unwrap()
            .into(),
    };
    let id = {
        let mut store = alt_task::Store::open(&f.main.join(".alt/task-data")).unwrap();
        store.publish(&intent).unwrap()
    };
    assert_eq!(
        value(&f.workspace, &["task", "integration-show", &id, "--json"])["state"],
        "unknown"
    );
    assert!(
        !run(
            &f.workspace,
            &["task", "integration-collect", &id, "--json"]
        )
        .status
        .success()
    );
    ok(
        &f.workspace,
        &[
            "task",
            "abandon",
            &f.task,
            "--reason",
            "stop unapplied attempt",
            "--json",
        ],
    );
    assert_eq!(
        value(&f.workspace, &["task", "show", &f.task, "--json"])["state"],
        "abandoned"
    );
    assert_eq!(std::fs::read(f.main.join("source.txt")).unwrap(), b"base\n");
}

#[test]
fn independent_intake_preserves_dirty_source_and_reports_unsupported_forms() {
    let temp = alt_os::temp::tempdir_in(std::env::var_os("HOME").unwrap()).unwrap();
    let source = temp.path().join("source");
    std::fs::create_dir(&source).unwrap();
    git(&source, &["init", "-b", "main"]);
    git(&source, &["config", "user.name", "fixture"]);
    git(&source, &["config", "user.email", "fixture@test"]);
    std::fs::write(source.join("file.txt"), "committed\n").unwrap();
    git(&source, &["add", "."]);
    git(&source, &["commit", "-m", "baseline"]);
    std::fs::write(source.join("file.txt"), "preexisting dirty\n").unwrap();
    std::fs::write(source.join(".env.local"), "excluded source secret\n").unwrap();
    let before = git(&source, &["show-ref"]);
    let index = std::fs::read(source.join(".git/index")).unwrap();
    let target = temp.path().join("native");
    let intake = value(
        temp.path(),
        &[
            "intake",
            source.to_str().unwrap(),
            target.to_str().unwrap(),
            "--git",
            git_path().to_str().unwrap(),
            "--json",
        ],
    );
    assert_eq!(
        intake["report"]["baseline"]["refs"]["HEAD"],
        "ref: refs/heads/main"
    );
    assert_eq!(
        std::fs::read(target.join("file.txt")).unwrap(),
        b"committed\n"
    );
    assert!(!target.join(".env.local").exists());
    assert!(!target.join(".git").exists());
    assert_eq!(
        std::fs::read(source.join("file.txt")).unwrap(),
        b"preexisting dirty\n"
    );
    assert_eq!(
        std::fs::read(source.join(".env.local")).unwrap(),
        b"excluded source secret\n"
    );
    assert_eq!(std::fs::read(source.join(".git/index")).unwrap(), index);
    assert_eq!(git(&source, &["show-ref"]), before);
    assert!(
        value(&target, &["status", "--json"])["unstaged"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    git(
        &source,
        &["config", "include.path", "/never-read-secret-config"],
    );
    let rejected = temp.path().join("rejected");
    let output = run(
        temp.path(),
        &[
            "intake",
            source.to_str().unwrap(),
            rejected.to_str().unwrap(),
            "--git",
            git_path().to_str().unwrap(),
            "--json",
        ],
    );
    assert_eq!(output.status.code(), Some(1));
    let failure: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(failure["error"]["outcome"], "not_applied");
    assert!(!rejected.exists());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("never-read-secret-config"));
}

/// Only this fixture's recorded child process group is signalled, including
/// its Git verifier. Drop also cleans up if an assertion fails.
struct ChildGroup(Option<std::process::Child>);
impl ChildGroup {
    fn spawn(root: &Path, args: &[&str]) -> Self {
        use std::os::unix::process::CommandExt;
        use std::process::Stdio;
        Self(Some(
            Command::new(env!("CARGO_BIN_EXE_alt"))
                .current_dir(root)
                .args(args)
                .env("ALT_NO_DAEMON", "1")
                .env("USER", "runner-test")
                .env("GIT_AUTHOR_NAME", "runner-test")
                .env("GIT_AUTHOR_EMAIL", "runner@test")
                .env_remove("ALT_PROTECTED_OVERRIDE")
                .process_group(0)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        ))
    }
    fn output_before(mut self, timeout: std::time::Duration) -> Output {
        let start = std::time::Instant::now();
        loop {
            if self.0.as_mut().unwrap().try_wait().unwrap().is_some() {
                return self.0.take().unwrap().wait_with_output().unwrap();
            }
            assert!(
                start.elapsed() < timeout,
                "query blocked on another task's verifier"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}
impl Drop for ChildGroup {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = alt_os::sys::kill(-(child.id() as i32), alt_os::sys::SIGKILL);
            let _ = child.wait();
        }
    }
}
#[test]
fn interrupted_git_verification_keeps_queries_available_and_never_publishes_partial_delivery() {
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, Instant};
    let f = fixture("printf verified\n");
    let evidence = observed(&f);
    let reviewed = review(&f, evidence["evidence_id"].as_str().unwrap());
    free_target(&f);
    let integrated = value(
        &f.workspace,
        &[
            "task",
            "integrate",
            reviewed["review_id"].as_str().unwrap(),
            "--json",
        ],
    );
    let integration = integrated["integration_id"].as_str().unwrap();
    let marker = f.scratch.path().join("verifier-started");
    let fifo = f.scratch.path().join("verifier-release");
    let fifo_c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { alt_os::sys::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
    let quote = |p: &Path| format!("'{}'", p.to_str().unwrap().replace('\'', "'\\''"));
    let wrapper = f.scratch.path().join("git-verifier");
    std::fs::write(&wrapper, format!("#!/bin/sh\ncase \" $* \" in *' fsck '*)\n printf ready > {}\n read release < {}\n ;;\nesac\nexec {} \"$@\"\n", quote(&marker), quote(&fifo), quote(&git_path()))).unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
    let target = f.scratch.path().join("interrupted-delivery");
    let request = "d0600000000000000000000000000009";
    let child = ChildGroup::spawn(
        &f.workspace,
        &[
            "--request-id",
            request,
            "task",
            "export",
            integration,
            target.to_str().unwrap(),
            "--git",
            wrapper.to_str().unwrap(),
            "--json",
        ],
    );
    let start = Instant::now();
    while !marker.exists() {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "verifier did not reach its explicit barrier"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let query = ChildGroup::spawn(&f.workspace, &["task", "show", &f.task, "--json"])
        .output_before(Duration::from_secs(3));
    assert!(
        query.status.success(),
        "{}",
        String::from_utf8_lossy(&query.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&query.stdout).unwrap()["state"],
        "integrated"
    );
    assert!(!target.exists());
    drop(child); // A real SIGKILL at the observed verification barrier.
    let pending = value(&f.workspace, &["request", "status", request, "--json"]);
    assert_eq!(pending["request"]["state"], "pending");
    assert_eq!(pending["request"]["outcome"], "unknown");
    assert!(
        !pending["request"]["operation_ids"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(!target.exists());
    assert!(!f.main.join(".alt/task-data/records/delivery").exists());
    // A new export can use the still-absent destination after the original
    // process is known to have stopped. The integration is never reapplied.
    let exported = value(
        &f.workspace,
        &[
            "task",
            "export",
            integration,
            target.to_str().unwrap(),
            "--git",
            git_path().to_str().unwrap(),
            "--json",
        ],
    );
    assert_eq!(exported["report"]["git"]["fsck_exit"], 0);
    assert_eq!(
        std::fs::read(target.join("source.txt")).unwrap(),
        b"reviewed\n"
    );
}

#[test]
fn non_deliverable_candidate_is_refused_before_review_or_integration() {
    for (path, bytes) in [
        (
            "source.txt",
            "version https://git-lfs.github.com/spec/v1\noid sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\nsize 123\n",
        ),
        (".gitattributes", "*.data filter=external\n"),
    ] {
        let mut f = fixture_with_paths("printf verified\n", &["source.txt", ".gitattributes"]);
        std::fs::write(f.workspace.join(path), bytes).unwrap();
        ok(&f.workspace, &["add", path]);
        f.candidate =
            value(&f.workspace, &["candidate", "create", &f.task, "--json"])["candidate_id"]
                .as_str()
                .unwrap()
                .into();
        let evidence = observed(&f);
        assert_eq!(evidence["validity"]["eligible"], true);
        let output = run(
            &f.workspace,
            &[
                "task",
                "review",
                &f.candidate,
                "--evidence",
                evidence["evidence_id"].as_str().unwrap(),
                "--reviewer",
                "owner",
                "--reason",
                "validation alone cannot grant unsupported delivery",
                "-m",
                "fix: unsupported input",
                "--json",
            ],
        );
        assert_eq!(
            output.status.code(),
            Some(1),
            "unsupported {path} was reviewed: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        let failure: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(failure["error"]["outcome"], "not_applied");
        assert!(!f.main.join(".alt/task-data/records/review").exists());
        assert!(!f.main.join(".alt/task-data/records/integration").exists());
        assert_eq!(
            value(&f.workspace, &["task", "show", &f.task, "--json"])["state"],
            "active"
        );
        // The public source HEAD may differ from the task workspace branch.
        // A new baseline must apply the same delivery checks to that checkout.
        std::fs::write(f.main.join(path), bytes).unwrap();
        ok(&f.main, &["add", path]);
        ok(&f.main, &["commit", "-m", "unsupported source baseline"]);
        let spec =
            value(&f.workspace, &["task", "show", &f.task, "--json"])["task"]["spec"].to_string();
        let refused_baseline = run(
            &f.workspace,
            &[
                "task",
                "create",
                "--definition",
                &spec,
                "--target",
                "refs/heads/main",
                "--json",
            ],
        );
        assert_eq!(
            refused_baseline.status.code(),
            Some(1),
            "unsupported baseline was accepted"
        );
        assert_eq!(
            value(&f.workspace, &["task", "list", "--json"])["tasks"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }
}
