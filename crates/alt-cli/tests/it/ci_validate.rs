//! `alt ci validate`: the M15/W47 schema-tier lint over a fixture
//! `.alt/ci/<name>/workflow.toml`. Three positive paths: well-formed
//! workflow → exit 0 + no output; schema-broken workflow → exit 1 +
//! diagnostic line citing the bad field; missing file → exit 0 +
//! "no workflow.toml files found" notice (no `.alt/ci` set up yet
//! is not an error — a fresh repo has nothing to validate).

use std::path::Path;
use std::process::{Command, Output};

fn alt(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_alt"))
        .current_dir(cwd)
        .env("ALT_NO_DAEMON", "1")
        .args(args)
        .output()
        .unwrap()
}

fn write_workflow(repo: &Path, name: &str, body: &str) {
    let dir = repo.join(".alt").join("ci").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("workflow.toml"), body).unwrap();
}

#[test]
fn alt_ci_validate_accepts_well_formed_workflow() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    std::fs::create_dir_all(repo.join(".alt").join("ci")).unwrap();
    write_workflow(
        repo,
        "build",
        r#"
[trigger]
ref_pattern = "refs/heads/main"
on = ["push"]

[[step]]
name = "build"
script = "scripts/build.sh"
agent = "ci-runner-linux-x86_64"

[[step]]
name = "test"
script = "scripts/test.sh"
needs = ["build"]

[artifacts]
build_out = "target/release/alt"
"#,
    );

    let out = alt(repo, &["ci", "validate"]);
    assert!(
        out.status.success(),
        "well-formed workflow must pass: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains(": error:"),
        "well-formed workflow must emit no errors, got: {stdout}"
    );
}

#[test]
fn alt_ci_validate_rejects_undefined_needs() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    write_workflow(
        repo,
        "broken",
        r#"
[[step]]
name = "build"
script = "scripts/build.sh"

[[step]]
name = "test"
script = "scripts/test.sh"
needs = ["does-not-exist"]
"#,
    );

    let out = alt(repo, &["ci", "validate"]);
    assert!(
        !out.status.success(),
        "broken workflow must exit non-zero: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("undefined step \"does-not-exist\""),
        "rejection must cite the missing step: {stdout}"
    );
}

#[test]
fn alt_ci_validate_emits_json_when_asked() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    write_workflow(
        repo,
        "cycle",
        r#"
[[step]]
name = "a"
script = "a.sh"
needs = ["b"]

[[step]]
name = "b"
script = "b.sh"
needs = ["a"]
"#,
    );

    let out = alt(repo, &["ci", "validate", "--json"]);
    assert!(
        !out.status.success(),
        "cycle must exit non-zero: stdout={}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    let mut had_cycle = false;
    for line in stdout.lines() {
        // Each diagnostic is one JSON object on its own line.
        assert!(
            line.starts_with('{') && line.ends_with('}'),
            "non-JSON line in --json output: {line}"
        );
        if line.contains("\"severity\":\"error\"") && line.contains("cycle") {
            had_cycle = true;
        }
    }
    assert!(had_cycle, "expected cycle error in JSON output: {stdout}");
}

#[test]
fn alt_ci_validate_handles_empty_ci_dir_gracefully() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let out = alt(repo, &["ci", "validate"]);
    assert!(
        out.status.success(),
        "missing .alt/ci must not be an error (fresh repo case): stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("no workflow.toml files found"),
        "expected the friendly no-workflow notice: {stdout}"
    );
}

#[test]
fn alt_ci_validate_accepts_explicit_path_argument() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("workflow.toml");
    std::fs::write(
        &path,
        r#"
[[step]]
name = "lone"
script = "lone.sh"
"#,
    )
    .unwrap();
    let out = alt(tmp.path(), &["ci", "validate", path.to_str().unwrap()]);
    assert!(
        out.status.success(),
        "explicit path well-formed must pass: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}
