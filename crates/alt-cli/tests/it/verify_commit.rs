//! M10/W16: `alt verify` CLI surface for commit-level alt-sig.

use std::path::Path;
use std::process::{Command, Output};

fn alt(repo: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_alt"))
        .current_dir(repo)
        .env("ALT_NO_DAEMON", "1")
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .env("USER", "tester")
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

fn last_commit_oid(repo: &Path) -> String {
    let log = ok(alt(repo, &["log", "-n", "1", "--pretty=oneline"]));
    log.split_whitespace().next().unwrap().to_owned()
}

/// Drop a generated Ed25519 sec key + sign-policy file so subsequent
/// `alt commit` calls splice an `alt-sig` header into the commit.
fn enable_signed_commits(repo: &Path, principal: &str) -> alt_sign::PublicKey {
    use alt_sign::SecretKey;
    let (sec, pubkey) = SecretKey::generate();
    let alt_dir = repo.join(".alt");
    std::fs::create_dir_all(alt_dir.join("identity")).unwrap();
    std::fs::write(
        alt_dir.join("identity").join(format!("{principal}.sec")),
        sec.to_text(),
    )
    .unwrap();
    std::fs::write(
        alt_dir.join("sign-policy"),
        format!("enabled = true\nprincipal = {principal}\n"),
    )
    .unwrap();
    pubkey
}

fn install_trust(repo: &Path, principal: &str, pubkey: &alt_sign::PublicKey) {
    let trust_dir = repo.join(".alt").join("trust");
    std::fs::create_dir_all(&trust_dir).unwrap();
    std::fs::write(trust_dir.join(format!("{principal}.pub")), pubkey.to_text()).unwrap();
}

#[test]
fn alt_verify_reports_signed_ok_when_trust_present() {
    let repo_dir = tempfile::tempdir().unwrap();
    let repo = repo_dir.path();
    ok(alt(repo, &["init", "."]));
    let pubkey = enable_signed_commits(repo, "alice");
    install_trust(repo, "alice", &pubkey);

    std::fs::write(repo.join("a.txt"), "alpha\n").unwrap();
    ok(alt(repo, &["add", "."]));
    ok(alt(repo, &["commit", "-m", "alice signed"]));
    let oid = last_commit_oid(repo);

    let out = ok(alt(repo, &["verify", &oid]));
    assert!(
        out.contains(&format!("{oid} signed-ok:alice")),
        "verify must report signed-ok with the alice principal: {out}"
    );
}

#[test]
fn alt_verify_reports_unsigned_when_no_alt_sig_header() {
    let repo_dir = tempfile::tempdir().unwrap();
    let repo = repo_dir.path();
    ok(alt(repo, &["init", "."]));
    // No sign-policy = no alt-sig header on the commit object.
    std::fs::write(repo.join("a.txt"), "alpha\n").unwrap();
    ok(alt(repo, &["add", "."]));
    ok(alt(repo, &["commit", "-m", "plain"]));
    let oid = last_commit_oid(repo);

    let out = ok(alt(repo, &["verify", &oid]));
    assert!(
        out.contains(&format!("{oid} unsigned")),
        "verify must report unsigned: {out}"
    );
}

#[test]
fn alt_verify_reports_untrusted_when_trust_missing() {
    let repo_dir = tempfile::tempdir().unwrap();
    let repo = repo_dir.path();
    ok(alt(repo, &["init", "."]));
    // sign but don't install the pubkey into trust/
    let _pubkey = enable_signed_commits(repo, "alice");
    std::fs::write(repo.join("a.txt"), "alpha\n").unwrap();
    ok(alt(repo, &["add", "."]));
    ok(alt(repo, &["commit", "-m", "signed-but-untrusted"]));
    let oid = last_commit_oid(repo);

    let out = ok(alt(repo, &["verify", &oid]));
    assert!(
        out.contains(&format!("{oid} untrusted:alice")),
        "verify must report untrusted when trust pubkey is absent: {out}"
    );
}

#[test]
fn alt_verify_walks_head_chain_when_no_oid_given() {
    let repo_dir = tempfile::tempdir().unwrap();
    let repo = repo_dir.path();
    ok(alt(repo, &["init", "."]));
    let pubkey = enable_signed_commits(repo, "alice");
    install_trust(repo, "alice", &pubkey);

    for i in 0..3 {
        std::fs::write(repo.join(format!("f{i}.txt")), format!("body-{i}\n")).unwrap();
        ok(alt(repo, &["add", "."]));
        ok(alt(repo, &["commit", "-m", &format!("c{i}")]));
    }

    let out = ok(alt(repo, &["verify"]));
    // every line should be a signed-ok row for alice
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 3, "verify must walk 3 commits: {out}");
    for line in &lines {
        assert!(
            line.contains("signed-ok:alice"),
            "every chain commit must verify: {line}"
        );
    }
}

#[test]
fn alt_verify_json_carries_the_same_verdict() {
    let repo_dir = tempfile::tempdir().unwrap();
    let repo = repo_dir.path();
    ok(alt(repo, &["init", "."]));
    let pubkey = enable_signed_commits(repo, "alice");
    install_trust(repo, "alice", &pubkey);
    std::fs::write(repo.join("a.txt"), "alpha\n").unwrap();
    ok(alt(repo, &["add", "."]));
    ok(alt(repo, &["commit", "-m", "json"]));
    let oid = last_commit_oid(repo);

    let out = ok(alt(repo, &["verify", "--json", &oid]));
    assert!(
        out.contains("\"verdict\":\"signed-ok\""),
        "json verdict: {out}"
    );
    assert!(
        out.contains("\"principal\":\"alice\""),
        "json principal: {out}"
    );
    assert!(
        out.contains(&format!("\"commit\":\"{oid}\"")),
        "json commit: {out}"
    );
}
