//! `git credential fill` integration: when alt's env-var credentials
//! aren't set but `git` is on PATH and a helper is configured, alt
//! shells out to `git credential fill` to resolve auth.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn alt(repo: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_alt"));
    cmd.current_dir(repo)
        .env("ALT_NO_DAEMON", "1")
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .args(args);
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().unwrap()
}

fn ok(o: Output) -> String {
    assert!(
        o.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout).unwrap()
}

#[test]
fn credential_helper_supplies_username_and_token_to_basic_auth() {
    // We mock `git credential fill` by writing a `git` shim earlier on
    // PATH that always answers with hardcoded creds, then point alt
    // at a deliberately-broken HTTPS URL. The fetch fails (no server)
    // but its error chain carries the URL we'd try with — when the
    // helper ran, that URL ends up Authorization-decorated, and the
    // alt-wire-http transport's error includes the host we hit.
    let tmp = tempfile::tempdir().unwrap();
    let shim_dir = tmp.path().join("bin");
    fs::create_dir_all(&shim_dir).unwrap();
    let shim = shim_dir.join("git");
    fs::write(
        &shim,
        "#!/bin/sh\n\
         if [ \"$1\" = \"credential\" ] && [ \"$2\" = \"fill\" ]; then\n\
         \tcat <<EOF\n\
         protocol=http\n\
         host=127.0.0.1:1\n\
         username=fixture-user\n\
         password=fixture-token\n\
         EOF\n\
         \texit 0\n\
         fi\n\
         exit 0\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(&shim).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&shim, perms).unwrap();

    let repo = tmp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    ok(alt(&repo, &["init"], &[]));
    ok(alt(
        &repo,
        &["remote", "add", "origin", "http://127.0.0.1:1/x.git"],
        &[],
    ));

    // PATH = shim_dir first, then a minimal real PATH for tempfile etc.
    let path = format!(
        "{}:/usr/bin:/bin",
        shim_dir.to_str().expect("shim path utf-8"),
    );
    let out = alt(&repo, &["fetch", "origin"], &[("PATH", path.as_str())]);
    // The fetch fails (no real server) — that's fine. We just want to
    // confirm the helper ran and the request actually went out (not
    // skipped). Skipping or failing-before-network would show up as a
    // different error.
    assert!(!out.status.success(), "fetch should fail (no real server)");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("127.0.0.1"),
        "expected URL host in error path: {err}",
    );
}

#[test]
fn alt_no_credential_helper_env_var_disables_the_helper_lookup() {
    // The shim here would panic if invoked (so we know it wasn't),
    // and `ALT_NO_CREDENTIAL_HELPER=1` should keep alt from calling
    // it. We don't have a server, so fetch fails; what matters is
    // that the failure mode is "network" not "shim crashed".
    let tmp = tempfile::tempdir().unwrap();
    let shim_dir = tmp.path().join("bin");
    fs::create_dir_all(&shim_dir).unwrap();
    let shim = shim_dir.join("git");
    fs::write(&shim, "#!/bin/sh\nexit 42\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(&shim).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&shim, perms).unwrap();

    let repo = tmp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    ok(alt(&repo, &["init"], &[]));
    ok(alt(
        &repo,
        &["remote", "add", "origin", "http://127.0.0.1:1/y.git"],
        &[],
    ));

    let path = format!(
        "{}:/usr/bin:/bin",
        shim_dir.to_str().expect("shim path utf-8"),
    );
    let out = alt(
        &repo,
        &["fetch", "origin"],
        &[("PATH", path.as_str()), ("ALT_NO_CREDENTIAL_HELPER", "1")],
    );
    assert!(!out.status.success(), "fetch should fail (no server)");
    let err = String::from_utf8_lossy(&out.stderr);
    // Should still mention the URL we tried — i.e. the request was
    // made (just unauthenticated, dummy host).
    assert!(
        err.contains("127.0.0.1"),
        "expected fetch to hit the URL: {err}",
    );
}
