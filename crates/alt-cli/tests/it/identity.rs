//! `alt identity init/list/trust` (M6/W7) — the local-side key surface
//! for op-level Ed25519 signing.

use std::path::Path;
use std::process::{Command, Output};

fn alt(repo: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_alt"))
        .current_dir(repo)
        .env("ALT_NO_DAEMON", "1")
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
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

/// `identity init <name>` writes the two key files with the right
/// `alt-…-ed25519:` prefixes, in the right directory; `.sec` is 0600.
#[test]
fn identity_init_writes_pub_and_sec_with_secret_perms() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    ok(alt(root, &["identity", "init", "alice"]));

    let pub_path = root.join(".alt/identity/alice.pub");
    let sec_path = root.join(".alt/identity/alice.sec");
    assert!(pub_path.is_file(), "pub key missing");
    assert!(sec_path.is_file(), "sec key missing");

    let pub_text = std::fs::read_to_string(&pub_path).unwrap();
    assert!(pub_text.starts_with("alt-pubkey-ed25519:"), "{pub_text}");
    let sec_text = std::fs::read_to_string(&sec_path).unwrap();
    assert!(sec_text.starts_with("alt-seckey-ed25519:"), "{sec_text}");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::metadata(&sec_path).unwrap().permissions();
        // mask off the file-type bits; only the rwx bits matter
        assert_eq!(perms.mode() & 0o777, 0o600, "sec key must be 0600");
    }
}

/// `identity init` refuses to overwrite an existing identity — a re-run
/// must not silently swap a principal's key.
#[test]
fn identity_init_refuses_to_overwrite_existing() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    ok(alt(root, &["identity", "init", "alice"]));
    let out = alt(root, &["identity", "init", "alice"]);
    assert!(!out.status.success(), "second init should fail");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("already exists"), "{err}");
}

/// `identity list` shows installed principals with fingerprints, and
/// flags `trusted` for ones whose pubkey also lives in the trust store.
#[test]
fn identity_list_and_trust_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    ok(alt(root, &["identity", "init", "alice"]));
    ok(alt(root, &["identity", "init", "bob"]));

    // before any trust call, neither is trusted
    let listed = ok(alt(root, &["identity", "list", "--json"]));
    assert!(listed.contains("\"principal\":\"alice\""), "{listed}");
    assert!(listed.contains("\"principal\":\"bob\""), "{listed}");
    assert!(!listed.contains("\"trusted\":true"), "{listed}");

    // trust alice via her own pub file (the obvious self-trust use case)
    let alice_pub = root.join(".alt/identity/alice.pub");
    ok(alt(
        root,
        &["identity", "trust", "alice", alice_pub.to_str().unwrap()],
    ));
    assert!(root.join(".alt/trust/alice.pub").is_file());

    let listed = ok(alt(root, &["identity", "list", "--json"]));
    // alice trusted, bob still not
    assert!(
        listed.contains("\"principal\":\"alice\"") && listed.contains("\"trusted\":true"),
        "{listed}"
    );
}

/// Trusting a non-key file (e.g. some random text) fails loudly — the
/// typo path doesn't silently install garbage into the trust store.
#[test]
fn identity_trust_rejects_non_pubkey_file() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    let bogus = root.join("not-a-key.txt");
    std::fs::write(&bogus, "hello there\n").unwrap();
    let out = alt(
        root,
        &["identity", "trust", "alice", bogus.to_str().unwrap()],
    );
    assert!(!out.status.success(), "should reject bad pubkey file");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("not a valid alt pubkey"), "{err}");
    assert!(!root.join(".alt/trust/alice.pub").exists());
}

/// Full A5b cycle: with `sign-policy` enabled and a local identity, every
/// ref tx writes a sidecar signature. `op-log --verify` reports
/// `signed-ok` for ops signed by a trusted principal and `unsigned` for
/// ops written before the policy was on. Tampering with a sig file flips
/// the verdict to `bad-sig`.
#[test]
fn op_log_verify_round_trips_signing_and_detects_tampering() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    ok(alt(root, &["identity", "init", "alice"]));
    let alice_pub = root.join(".alt/identity/alice.pub");
    ok(alt(
        root,
        &["identity", "trust", "alice", alice_pub.to_str().unwrap()],
    ));

    // turn on signing for principal `alice`
    std::fs::write(
        root.join(".alt/sign-policy"),
        "enabled = true\nprincipal = alice\n",
    )
    .unwrap();

    // also tell the binary to act as `alice` so the implicit caller-id
    // matches what we trusted
    let mut env = std::collections::HashMap::new();
    env.insert("ALT_PRINCIPAL_ID", "alice");
    let alt_as_alice = |args: &[&str]| -> Output {
        Command::new(env!("CARGO_BIN_EXE_alt"))
            .current_dir(root)
            .env("ALT_NO_DAEMON", "1")
            .env("GIT_AUTHOR_NAME", "alice")
            .env("GIT_AUTHOR_EMAIL", "a@e")
            .env("ALT_PRINCIPAL_ID", "alice")
            .env("USER", "alice")
            .args(args)
            .output()
            .unwrap()
    };

    std::fs::write(root.join("hello.txt"), "hi\n").unwrap();
    ok(alt_as_alice(&["add", "."]));
    ok(alt_as_alice(&["commit", "-m", "first"]));

    // sigs.log now has one entry for the commit ref tx
    let sigs_path = root.join(".alt/oplog/sigs.log");
    assert!(
        sigs_path.is_file(),
        "sigs sidecar should exist after commit"
    );
    let sigs_body = std::fs::read_to_string(&sigs_path).unwrap();
    assert!(
        sigs_body.contains("alt-sig-ed25519:"),
        "sigs.log: {sigs_body}"
    );
    assert!(sigs_body.contains(" alice "), "sigs.log: {sigs_body}");

    // verify-mode JSON reports signed-ok for the signed ops and unsigned
    // for the init op (which preceded the sign-policy file)
    let json = ok(alt(root, &["op-log", "--verify", "--json"]));
    assert!(
        json.contains("\"status\":\"signed-ok\"") && json.contains("\"principal\":\"alice\""),
        "signed-ok missing: {json}"
    );
    assert!(
        json.contains("\"status\":\"unsigned\""),
        "init op should report unsigned: {json}"
    );

    // human view also tags rows
    let human = ok(alt(root, &["op-log", "--verify"]));
    assert!(
        human.contains("sig=signed-ok:alice"),
        "human verify rows: {human}"
    );

    // tamper with the sig file: flip a base64 char inside the last
    // signature, which should make verify return `bad-sig`
    let mut tampered = sigs_body.clone();
    if let Some(pos) = tampered.find("alt-sig-ed25519:") {
        let body_start = pos + "alt-sig-ed25519:".len();
        // flip the first body byte: 'A' <-> 'B', or in general bump it
        // to the next alphabet letter
        let bytes = unsafe { tampered.as_bytes_mut() };
        bytes[body_start] = match bytes[body_start] {
            b'A' => b'B',
            b'B' => b'A',
            other if other.is_ascii_alphabetic() => b'A',
            other => other,
        };
    }
    std::fs::write(&sigs_path, &tampered).unwrap();
    let json2 = ok(alt(root, &["op-log", "--verify", "--json"]));
    assert!(
        json2.contains("\"status\":\"bad-sig\""),
        "tampered sig should report bad-sig: {json2}"
    );
}

/// Without `<alt-dir>/sign-policy`, signing is off — no sigs file gets
/// created, and `op-log --verify` reports every op as `unsigned`.
#[test]
fn op_log_verify_is_unsigned_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("a.txt"), "hi\n").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "first"]));

    assert!(
        !root.join(".alt/oplog/sigs.log").exists(),
        "sigs.log should not be created when sign-policy is off"
    );
    let json = ok(alt(root, &["op-log", "--verify", "--json"]));
    assert!(
        json.contains("\"status\":\"unsigned\""),
        "every op should be unsigned: {json}"
    );
    assert!(
        !json.contains("\"status\":\"signed-ok\""),
        "nothing should be signed-ok: {json}"
    );
}

/// Invalid principal ids are rejected at the entry point so a hostile
/// name can't escape the `<alt-dir>/identity/` directory.
#[test]
fn identity_init_rejects_invalid_principal_ids() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    for bad in ["..", ".hidden", "with/slash", "with space"] {
        let out = alt(root, &["identity", "init", bad]);
        assert!(
            !out.status.success(),
            "should reject {bad:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
