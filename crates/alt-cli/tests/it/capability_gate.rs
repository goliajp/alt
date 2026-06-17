//! A6 (C3): the four capability gates — read-only, branch namespace, force,
//! path — each deny their respective overreach with a clear error and *zero
//! on-disk side effect*. A repo with no `.alt/policy` file behaves byte-for-
//! byte as before — the gates are silent until policy says otherwise.

use std::path::Path;
use std::process::{Command, Output};

fn alt_as(repo: &Path, args: &[&str], principal_id: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_alt"))
        .current_dir(repo)
        .env("ALT_NO_DAEMON", "1")
        .env("GIT_AUTHOR_NAME", principal_id)
        .env("GIT_AUTHOR_EMAIL", format!("{principal_id}@e"))
        .env("USER", "operator")
        .env("ALT_PRINCIPAL_KIND", "agent")
        .env("ALT_PRINCIPAL_ID", principal_id)
        .args(args)
        .output()
        .unwrap()
}

/// As the unrestricted human operator (no ALT_PRINCIPAL_* set; defaults to
/// Human/USER, which is "operator" — the policies tested below target
/// `agent:*`, so this principal is unconstrained).
fn alt(repo: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_alt"))
        .current_dir(repo)
        .env("ALT_NO_DAEMON", "1")
        .env("GIT_AUTHOR_NAME", "operator")
        .env("GIT_AUTHOR_EMAIL", "op@e")
        .env("USER", "operator")
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

fn fails_with(o: Output, needle: &str) -> String {
    let err = String::from_utf8_lossy(&o.stderr).to_string();
    assert!(
        !o.status.success(),
        "expected failure, stdout was: {}",
        String::from_utf8_lossy(&o.stdout)
    );
    assert!(
        err.contains(needle),
        "stderr does not contain {needle:?}: {err}"
    );
    err
}

/// One-shot fixture: init repo, write base file, stage + commit once as the
/// unrestricted operator. Returns the repo path (kept alive by the tempdir).
fn fixture(tmp: &tempfile::TempDir) -> &Path {
    let repo = tmp.path();
    ok(alt(repo, &["init", "."]));
    std::fs::write(repo.join("a.txt"), "base\n").unwrap();
    ok(alt(repo, &["add", "."]));
    ok(alt(repo, &["commit", "-m", "base"]));
    repo
}

fn write_policy(repo: &Path, body: &str) {
    std::fs::write(repo.join(".alt/policy"), body).unwrap();
}

/// `read-only` denies any write — index (`add`) *and* refs (`commit`,
/// `branch`). Without that gate, `add` would succeed (it only touches the
/// index) and only the ref tx would fail; the read-only check at the top of
/// each write command catches the index path too. The same policy is
/// silent on reads (separate test below).
#[test]
fn read_only_denies_every_write() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = fixture(&tmp);
    write_policy(repo, "agent:* -> read-only\n");

    std::fs::write(repo.join("b.txt"), "more\n").unwrap();
    fails_with(alt_as(repo, &["add", "."], "rover"), "capability denied");
    fails_with(
        alt_as(repo, &["branch", "feat"], "rover"),
        "capability denied",
    );
    // the unrestricted operator (no rule targets `human:operator`) still
    // writes — proves the rule applies *per principal*, not globally.
    ok(alt(repo, &["branch", "still-ok"]));
}

/// Reads are never gated, regardless of how strict the policy is. The same
/// read-only rule from the previous test must let status/log/branch-listing
/// through for the restricted principal.
#[test]
fn read_only_does_not_gate_reads() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = fixture(&tmp);
    write_policy(repo, "agent:* -> read-only\n");
    ok(alt_as(repo, &["status"], "rover"));
    ok(alt_as(repo, &["log"], "rover"));
    ok(alt_as(repo, &["branch"], "rover"));
}

/// `branch_allow` restricts which ref names a principal may write. A branch
/// outside the allowed namespace is denied — and the underlying op log gains
/// no entry (the gate fires inside the append lock, before validate).
#[test]
fn branch_allow_denies_out_of_namespace() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = fixture(&tmp);
    write_policy(repo, "agent:* -> branch=refs/heads/feature/agent-bot/**\n");

    // the bot agent is restricted to its namespace…
    fails_with(
        alt_as(repo, &["branch", "main-rewrite"], "bot"),
        "capability denied",
    );
    ok(alt_as(repo, &["branch", "feature/agent-bot/wip"], "bot"));

    // …and the unrestricted operator (no rule matches `human:operator`)
    // still moves freely
    ok(alt(repo, &["branch", "other-branch"]));
}

/// `forbid-force` denies branch deletion (the canonical force op) and any
/// non-fast-forward update. Creations stay allowed: nothing of value is
/// being overwritten.
#[test]
fn forbid_force_blocks_delete_and_keeps_creations_free() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = fixture(&tmp);
    // first put a branch in place under the unrestricted operator
    ok(alt(repo, &["branch", "scratch"]));

    write_policy(repo, "agent:* -> forbid-force\n");

    // delete = force op → denied
    fails_with(
        alt_as(repo, &["branch", "-d", "scratch"], "bot"),
        "capability denied",
    );
    // create = not force → allowed
    ok(alt_as(repo, &["branch", "agent-fresh"], "bot"));

    // the protected branch is still around
    let listing = ok(alt(repo, &["branch"]));
    assert!(
        listing.contains("scratch"),
        "scratch should still be there after a denied delete: {listing}"
    );
}

/// `path_allow` blocks staging files outside the allowed path globs. The check
/// fires inside `add` (before the blob is even written to the odb) and again
/// inside `commit` (so an index-from-before-policy can't sneak past).
#[test]
fn path_allow_denies_out_of_tree_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = fixture(&tmp);
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::create_dir_all(repo.join("scripts")).unwrap();
    std::fs::write(repo.join("src/lib.rs"), "// src\n").unwrap();
    std::fs::write(repo.join("scripts/run.sh"), "#!/bin/sh\n").unwrap();

    write_policy(repo, "agent:* -> path=src/**\n");

    // restricted agent can stage src/ but not scripts/
    ok(alt_as(repo, &["add", "src/lib.rs"], "bot"));
    fails_with(
        alt_as(repo, &["add", "scripts/run.sh"], "bot"),
        "capability denied",
    );
    // commit of the already-staged src/ is fine
    ok(alt_as(repo, &["commit", "-m", "add src"], "bot"));
}

/// The zero-regression red line: a repo with no `.alt/policy` file behaves
/// exactly as before C3. Every command — including the writes the gates
/// could touch — must succeed for any principal.
#[test]
fn missing_policy_is_a_full_capabilities_default() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = fixture(&tmp);
    assert!(
        !repo.join(".alt/policy").exists(),
        "fixture must not have written a policy"
    );

    // every flavour of write a restricted policy could touch — none gated
    ok(alt_as(repo, &["branch", "main-rewrite"], "bot"));
    std::fs::write(repo.join("b.txt"), "more\n").unwrap();
    ok(alt_as(repo, &["add", "."], "bot"));
    ok(alt_as(repo, &["commit", "-m", "more"], "bot"));
    ok(alt_as(repo, &["branch", "-d", "main-rewrite"], "bot"));
}

/// C4: a denied write with `--json` reports a structured JSON error on
/// stderr (kind = "capability_denied") so an agent can detect the denial
/// without parsing the human "fatal: …" string. Exit code is non-zero.
#[test]
fn json_invocation_surfaces_capability_denied_as_structured_error() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = fixture(&tmp);
    write_policy(repo, "agent:* -> read-only\n");

    let out = alt_as(repo, &["branch", "feat", "--json"], "rover");
    assert!(!out.status.success(), "expected denial");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("\"kind\":\"capability_denied\""),
        "structured error kind missing: {stderr}"
    );
    assert!(
        stderr.contains("\"schema_version\":1"),
        "structured error schema_version missing: {stderr}"
    );
    // and the human path is unchanged: same command without --json prints the
    // free-form "fatal: …" line that the rest of the CLI uses.
    let plain = alt_as(repo, &["branch", "feat"], "rover");
    let plain_err = String::from_utf8_lossy(&plain.stderr);
    assert!(
        plain_err.contains("fatal: capability denied"),
        "human error path changed: {plain_err}"
    );
}

/// First-match-wins: a specific allow rule above a broad deny must shadow it.
/// `agent:bot` gets its own ref namespace, while `agent:*` is read-only.
/// This exercises the lookup ordering end-to-end (it has unit tests, but a
/// command-level test guarantees the chain — policy → caps → gate — is
/// wired and lookup is per-request, not cached against a stale principal).
#[test]
fn specific_rule_above_catch_all_wins_per_principal() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = fixture(&tmp);
    write_policy(
        repo,
        "agent:bot -> branch=refs/heads/feature/bot/**\n\
         agent:*      -> read-only\n",
    );

    // the bot lands its own branch under feature/bot/…
    ok(alt_as(repo, &["branch", "feature/bot/wip"], "bot"));
    // …but the catch-all read-only rule does apply to every other agent
    fails_with(
        alt_as(repo, &["branch", "anywhere"], "rover"),
        "capability denied",
    );
}
