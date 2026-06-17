//! `alt op-log`: the A5a audit-view command. Closes the structured-identity
//! story (C1 wrote identities into the op log; this reads them back out).

use std::path::Path;
use std::process::{Command, Output};

fn alt(repo: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_alt"))
        .current_dir(repo)
        .env("ALT_NO_DAEMON", "1")
        .env("GIT_AUTHOR_NAME", "alice")
        .env("GIT_AUTHOR_EMAIL", "alice@e")
        .env("USER", "alice")
        .args(args)
        .output()
        .unwrap()
}

fn alt_as_agent(repo: &Path, args: &[&str], agent_id: &str, session: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_alt"))
        .current_dir(repo)
        .env("ALT_NO_DAEMON", "1")
        .env("GIT_AUTHOR_NAME", agent_id)
        .env("GIT_AUTHOR_EMAIL", format!("{agent_id}@e"))
        .env("USER", "operator")
        .env("ALT_PRINCIPAL_KIND", "agent")
        .env("ALT_PRINCIPAL_ID", agent_id)
        .env("ALT_SESSION_ID", session)
        .args(args)
        .output()
        .unwrap()
}

fn ok(o: Output) -> String {
    assert!(
        o.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout).unwrap()
}

/// The human view lists ops newest-first with the parsed principal (kind:id
/// and session), the verb, and one indented line per ref change inside a
/// ref-transaction payload. Other op kinds appear without ref-change lines
/// — the audit trail is complete either way.
#[test]
fn op_log_lists_recent_ops_newest_first_with_parsed_principal() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("a.txt"), "v1\n").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "base"]));
    // a second op carrying an agent principal and a session correlator
    ok(alt_as_agent(root, &["branch", "feat"], "bot-1", "s-77"));

    let view = ok(alt(root, &["op-log"]));
    // newest first: the branch op should appear above the commit op
    let branch_pos = view
        .find("verb=branch")
        .unwrap_or_else(|| panic!("no branch op: {view}"));
    let commit_pos = view
        .find("verb=commit")
        .unwrap_or_else(|| panic!("no commit op: {view}"));
    assert!(
        branch_pos < commit_pos,
        "branch should be newer than commit: {view}"
    );
    // parsed principal of the agent op carries id + session
    assert!(view.contains("agent:bot-1"), "{view}");
    assert!(view.contains("session=s-77"), "{view}");
    // the commit op is from the default Human principal (USER=alice)
    assert!(view.contains("human:alice"), "{view}");
    // a ref-tx payload shows one indented `ref` line per change
    assert!(
        view.contains("  ref refs/heads/feat:"),
        "missing feat ref change: {view}"
    );
}

/// `-n` caps the count, newest first.
#[test]
fn op_log_n_limits_the_entry_count() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("a.txt"), "x\n").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "base"]));
    ok(alt(root, &["branch", "b1"]));
    ok(alt(root, &["branch", "b2"]));
    ok(alt(root, &["branch", "b3"]));

    let view = ok(alt(root, &["op-log", "-n", "2"]));
    let entries = view.matches("verb=").count();
    assert_eq!(entries, 2, "want 2 entries, got: {view}");
    // newest first: b3 is the latest
    assert!(view.contains("  ref refs/heads/b3:"), "{view}");
}

/// `--json` emits a structured doc agents can consume directly: parsed
/// principal, verb, and the ref_changes array (null when not a ref tx).
#[test]
fn op_log_json_carries_structured_audit_entries() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("a.txt"), "x\n").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "base"]));
    ok(alt_as_agent(root, &["branch", "feat"], "rover", "sess-1"));

    let json = ok(alt(root, &["op-log", "--json"]));
    assert!(json.contains("\"schema_version\":1"), "{json}");
    assert!(json.contains("\"ops\":["), "{json}");
    // newest entry: agent rover making a ref change to refs/heads/feat
    assert!(
        json.contains("\"principal\":{\"kind\":\"agent\",\"id\":\"rover\",\"session\":\"sess-1\"}"),
        "principal not surfaced: {json}"
    );
    assert!(json.contains("\"verb\":\"branch\""), "{json}");
    assert!(
        json.contains("\"ref_changes\":[{\"name\":\"refs/heads/feat\""),
        "ref_changes missing: {json}"
    );
}
