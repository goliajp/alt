use super::{Scene, member};
use alt_json::Value;

const BINDINGS: &str = r#"[{"name":"bash","path":"/bin/bash","version_args":["--version"]}]"#;
const ENVIRONMENT: &str = r#"{"PATH":"/usr/bin:/bin"}"#;

pub fn definition(tools: &Value, purpose: &str) -> String {
    format!(
        r#"{{"purpose":"{purpose}","authority":"test-owner","allowed_paths":["source.txt"],"auxiliary":[{{"path":".ignored","purpose":"private probe"}},{{"path":"probe.txt","purpose":"root probe"}}],"exclusions":[],"validation":{{"commands":[["bash","gate.sh"]],"cwd":".","environment":{{"values":{ENVIRONMENT},"complete":true}},"tools":{tools},"external":[],"validation":{{"outputs":["target"]}}}}}}"#
    )
}

/// A repository in `main`, an isolated workspace in `task`, and the tools a
/// runner on this machine reports. Leaves the scene in `task`.
pub fn prepared(name: &'static str, script: &str) -> (Scene, Value) {
    let mut s = Scene::new(name);
    // what the machine's own tools and the scene's location decide
    // and a map keyed by commit ids, whose order follows the commit times
    s.volatile(&["runner", "version", "bytes", "objects"]);
    s.alias(env!("CARGO_BIN_EXE_alt"), "alt");
    s.cd("main");
    s.setup(&["init", "."]);
    s.write("source.txt", "base\n");
    s.write("gate.sh", script);
    s.write(".gitignore", ".ignored/\nprobe.txt\ntarget/\n");
    s.setup(&["add", "."]);
    s.setup(&["commit", "-m", "base"]);
    let workspace = s.text("../task");
    s.setup(&["workspace", "add", "task", &workspace]);
    s.cd("scratch");
    let probe = s.json(&[
        "runner",
        "probe",
        "--bindings",
        BINDINGS,
        "--environment",
        ENVIRONMENT,
        "--json",
    ]);
    let probe: Value = alt_json::from_slice(&probe.stdout).unwrap();
    s.cd("task");
    s.write(".ignored/probe", "private probe\n");
    s.write("probe.txt", "root probe\n");
    (s, probe["tools"].clone())
}

pub fn profile(s: &Scene) -> String {
    format!(
        r#"{{"kind":"local","scratch":"{}","tools":{BINDINGS},"timeout_ms":20000}}"#,
        s.text("../scratch")
    )
}

#[test]
fn task_from_creation_to_delivery() {
    let (mut s, tools) = prepared(
        "task-delivery",
        "test \"$(cat source.txt)\" = reviewed\nprintf 'verified \\377\\n'\nprintf diagnostic >&2\n",
    );
    let created = s.json(&[
        "task",
        "create",
        "--definition",
        &definition(&tools, "validate isolated snapshot"),
        "--target",
        "refs/heads/main",
        "--json",
    ]);
    let task = member(&created, "task_id");
    s.json(&["task", "list", "--json"]);
    s.json(&["task", "show", &task, "--json"]);
    s.json(&["task", "show", &task]);
    s.write("source.txt", "reviewed\n");
    s.setup(&["add", "source.txt"]);
    let candidate = member(
        &s.json(&["candidate", "create", &task, "--json"]),
        "candidate_id",
    );
    s.json(&["candidate", "show", &candidate, "--json"]);
    s.json(&["candidate", "check", &candidate, "--json"]);
    let profile = profile(&s);
    let observed = s.json(&[
        "evidence",
        "run",
        &candidate,
        "--profile",
        &profile,
        "--json",
    ]);
    let evidence = member(&observed, "evidence_id");
    let attempt = member(&observed, "attempt_id");
    s.json(&["evidence", "show", &evidence, "--json"]);
    s.json(&["evidence", "check", &evidence, "--json"]);
    s.json(&["evidence", "log", &evidence, "stdout", "--json"]);
    s.json(&["evidence", "log", &evidence, "stderr"]);
    s.json(&["evidence", "run-status", &attempt, "--json"]);
    s.json(&["evidence", "collect", &attempt, "--json"]);
    s.json(&["task", "changes", &candidate, "--json"]);
    let review = member(
        &s.json(&[
            "task",
            "review",
            &candidate,
            "--evidence",
            &evidence,
            "--reviewer",
            "fixture-owner",
            "--reason",
            "inspected full change",
            "-m",
            "fix: deliver reviewed source",
            "--json",
        ]),
        "review_id",
    );
    s.json(&["task", "review-show", &review, "--json"]);
    s.json(&["task", "integrate", &review, "--json"]);
    s.cd("main");
    s.setup(&["switch", "-c", "target-hold"]);
    s.cd("task");
    let request = "d0600000000000000000000000000001";
    let integrate = [
        "--request-id",
        request,
        "task",
        "integrate",
        &review,
        "--json",
    ];
    let integration = member(&s.json(&integrate), "integration_id");
    s.json(&integrate);
    s.json(&["task", "integration-show", &integration, "--json"]);
    s.json(&["task", "integration-collect", &integration, "--json"]);
    s.json(&["task", "show", &task, "--json"]);
    let output = s.text("../delivered");
    let delivery = member(
        &s.json(&["task", "export", &integration, &output, "--json"]),
        "delivery_id",
    );
    s.json(&["task", "delivery-show", &delivery, "--json"]);
    s.json(&["task", "delivery-collect", &delivery, "--json"]);
    s.json(&["task", "list", "--json"]);
    s.json(&["request", "pending", "--json"]);
    s.json(&["request", "result", request, "--json"]);
    s.json(&["op-log", "--json"]);
    s.finish();
}

#[test]
fn imported_evidence_failed_run_and_abandonment() {
    let (mut s, tools) = prepared("task-evidence", "printf out\nexit 7\n");
    let task = member(
        &s.json(&[
            "task",
            "create",
            "--definition",
            &definition(&tools, "second \\\"task\\\" — 日本語"),
            "--target",
            "refs/heads/main",
            "--json",
        ]),
        "task_id",
    );
    s.write("source.txt", "candidate\n");
    s.setup(&["add", "source.txt"]);
    let created = s.json(&["candidate", "create", &task, "--json"]);
    let candidate = member(&created, "candidate_id");
    let created: Value = alt_json::from_slice(&created.stdout).unwrap();
    let manifest = created["candidate"]["manifest"].as_str().unwrap();
    let shown = s.json(&["candidate", "show", &candidate, "--json"]);
    let shown: Value = alt_json::from_slice(&shown.stdout).unwrap();
    let profile = profile(&s);
    s.json(&[
        "evidence",
        "run",
        &candidate,
        "--profile",
        &profile,
        "--json",
    ]);
    s.json(&[
        "evidence",
        "run",
        &candidate,
        "--profile",
        "{\"kind\":\"elsewhere\"}",
        "--json",
    ]);
    s.json(&[
        "evidence",
        "run",
        &candidate,
        "--profile",
        "{\"kind\":\"local\"}",
        "--json",
    ]);
    s.write("../output.bin", b"raw \xff\0\nlog");
    let report = format!(
        r#"{{"schema_version":1,"candidate":"{candidate}","manifest":"{manifest}","before":"{manifest}","after":"{manifest}","runner":"external","actual_context":{},"binaries":{{}},"started_unix_ns":1,"finished_unix_ns":2,"termination":{{"kind":"exit","code":0}},"stdout":null,"stderr":null,"artifacts":{{}},"provenance":"observed","original_report":null,"creator_request":"external-claim"}}"#,
        shown["input_manifest"]["context"]
    );
    let output = s.text("../output.bin");
    let imported = s.json(&[
        "evidence", "import", &candidate, "--report", &report, "--stdout", &output, "--json",
    ]);
    let evidence = member(&imported, "evidence_id");
    s.json(&["evidence", "show", &evidence, "--json"]);
    s.json(&["evidence", "check", &evidence, "--json"]);
    s.json(&["evidence", "log", &evidence, "stdout"]);
    s.json(&["evidence", "log", &evidence, "original-report"]);
    s.json(&[
        "evidence",
        "import",
        &candidate,
        "--report",
        "{\"schema_version\":1}",
        "--json",
    ]);
    s.write("source.txt", "moved on\n");
    s.setup(&["add", "source.txt"]);
    s.json(&["candidate", "check", &candidate, "--json"]);
    s.json(&[
        "candidate",
        "create",
        &task,
        "--parent",
        &candidate,
        "--json",
    ]);
    s.json(&[
        "task",
        "abandon",
        &task,
        "--reason",
        "no longer needed",
        "--json",
    ]);
    s.json(&["task", "show", &task, "--json"]);
    s.json(&["candidate", "create", &task, "--json"]);
    s.json(&["task", "list", "--json"]);
    s.finish();
}
