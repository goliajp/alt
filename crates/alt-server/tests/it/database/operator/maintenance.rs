use super::*;
use std::os::unix::fs::PermissionsExt;

#[test]
#[ignore = "requires isolated PostgreSQL, age and matching PostgreSQL tools; real Cloud backup and fresh runtime binding"]
fn cloud_backup_restores_with_fresh_binding_and_rejects_old_credentials() {
    use golia_platform_contract::{
        AccountAuthorization, Audience, Capability, Counter, Lease, Product,
    };
    let source = OperatorDb::new();
    let configure = |db: &OperatorDb, generation: u64, key: &str| {
        let mut config = Config::read(&db.config).unwrap();
        config.instance.deployment_mode = DeploymentMode::Cloud;
        config.instance.runtime_generation = generation;
        let audience = Audience {
            organization_id: "organization".to_string().try_into().unwrap(),
            workspace_id: config.instance.workspace_id.clone().try_into().unwrap(),
            installation_id: config.instance.installation_id.clone().try_into().unwrap(),
            product: Product::Alt,
            generation: Counter(generation),
            origin: "https://alt.example.test".to_string().try_into().unwrap(),
            control_origin: "https://cloud.example.test".to_string().try_into().unwrap(),
        };
        let credential = db.root.path().join("runtime.key");
        std::fs::write(&credential, key).unwrap();
        std::fs::set_permissions(&credential, std::fs::Permissions::from_mode(0o600)).unwrap();
        config.cloud = Some(alt_server::operator::CloudConfig {
            audience,
            runtime_credential_file: credential,
            provision_digest: Some(
                blake3::hash(format!("spec-{generation}").as_bytes())
                    .to_hex()
                    .to_string(),
            ),
        });
        std::fs::write(&db.config, serde_json::to_vec(&config).unwrap()).unwrap();
        config
    };
    let config = configure(&source, 1, &"ab".repeat(32));
    source.ok(&[
        "secret",
        "receipt-key",
        "--output",
        &source.path("receipt.key"),
    ]);
    source.ok(&["initialize"]);
    let mut store = Store::open(
        Client::connect(&source.dsn, NoTls).unwrap(),
        ByteStore::open(&config.bytes_directory).unwrap(),
        config.instance.clone(),
        config.signing_key().unwrap(),
        limits(),
    )
    .unwrap();
    let claims = AccountAuthorization {
        lease: Lease {
            version: 1,
            audience: config.cloud.as_ref().unwrap().audience.clone(),
            sequence: Counter(1),
            revocation_generation: Counter(1),
            issued_at_unix_ms: Counter(future() - 3_600_000),
            expires_at_unix_ms: Counter(future() - 3_540_000),
        },
        account_id: "owner".to_string().try_into().unwrap(),
        session_id: "session".to_string().try_into().unwrap(),
        membership_id: "member".to_string().try_into().unwrap(),
        display_name: "Owner".into(),
        capabilities: vec![Capability::UseProduct, Capability::ManageInstance],
    };
    let token = alt_browser_auth::token();
    store
        .accept_cloud_login(&token, "sealed-grant", &claims, future(), true)
        .unwrap();
    assert!(store.authenticate(&token).is_ok());
    let trust = source.ok(&["health"])["receipt_public_key"]
        .as_str()
        .unwrap()
        .to_owned();
    let external = tempfile::tempdir().unwrap();
    let (identity, recipient) = encryption_key(external.path());
    let backup = external.path().join("cloud-backup");
    let input = backup.to_str().unwrap();
    source.ok(&["backup", "--recipient", &recipient, "--output", input]);
    let target = OperatorDb::new();
    let restored_config = configure(&target, 10, &"cd".repeat(32));
    let result = target.ok(&[
        "restore",
        "--input",
        input,
        "--identity",
        &identity,
        "--trusted-receipt-key",
        &trust,
        "--maximum-bytes",
        "100000000",
        "--source-fenced",
    ]);
    assert_eq!(result["old_credentials_revoked"], true);
    assert_eq!(target.ok(&["health"])["ready"], true);
    let mut restored = Store::open(
        Client::connect(&target.dsn, NoTls).unwrap(),
        ByteStore::open(&restored_config.bytes_directory).unwrap(),
        restored_config.instance.clone(),
        restored_config.signing_key().unwrap(),
        limits(),
    )
    .unwrap();
    assert!(restored.authenticate(&token).is_err());
    let runtime = restored_config.cloud_runtime().unwrap().unwrap();
    runtime.check(&mut restored).unwrap();
    assert!(
        config
            .cloud_runtime()
            .unwrap()
            .unwrap()
            .check(&mut restored)
            .is_err()
    );
    let mut fresh = claims;
    fresh.lease.audience = restored_config.cloud.unwrap().audience;
    let fresh_token = alt_browser_auth::token();
    restored
        .accept_cloud_login(&fresh_token, "fresh-envelope", &fresh, future(), true)
        .unwrap();
    assert!(restored.authenticate(&fresh_token).is_ok());
    assert!(
        store.authenticate(&token).is_ok(),
        "isolated restore does not itself fence the source"
    );
    assert_eq!(source.ok(&["health"])["instance"]["runtime_generation"], 1);
    // The executor-facing command reconciles a lost completion response without
    // importing the archive again or overwriting a completed target.
    use golia_platform_contract::lifecycle::{Images, ProvisionSpec, RecoveryBackup, RecoverySpec};
    let verified = alt_server::operator::backup::verify(
        &backup,
        std::path::Path::new(&identity),
        config.signing_key().unwrap().verifying_key().to_bytes(),
        100_000_000,
    )
    .unwrap();
    let source_spec = ProvisionSpec {
        version: 1,
        audience: config.cloud.as_ref().unwrap().audience.clone(),
        region: "region".to_string().try_into().unwrap(),
        images: Images {
            application: format!("sha256:{}", "a".repeat(64)),
            database: format!("sha256:{}", "b".repeat(64)),
            edge: Some(format!("sha256:{}", "c".repeat(64))),
        },
        runtime_key: "ab".repeat(32).try_into().unwrap(),
        data_key: "01".repeat(32).try_into().unwrap(),
        database_password: "02".repeat(32).try_into().unwrap(),
    };
    let mut next_spec = source_spec.clone();
    next_spec.audience.generation = Counter(20);
    next_spec.runtime_key = "03".repeat(32).try_into().unwrap();
    next_spec.data_key = "04".repeat(32).try_into().unwrap();
    next_spec.database_password = "05".repeat(32).try_into().unwrap();
    let recovery = RecoverySpec {
        version: 1,
        operation_id: "restore-operation".to_string().try_into().unwrap(),
        source: source_spec,
        target: next_spec.clone(),
        backup: RecoveryBackup {
            manifest_digest: verified.manifest_digest,
            receipt_public_key: trust,
            generation: Counter(1),
            maximum_bytes: Counter(100_000_000),
        },
    };
    let managed = OperatorDb::new();
    let state = managed.root.path().join("state");
    let assets = managed.root.path().join("assets");
    std::fs::create_dir(&state).unwrap();
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::create_dir(&assets).unwrap();
    std::fs::write(
        assets.join("index.html"),
        "<!doctype html><title>Alt</title>",
    )
    .unwrap();
    let spec_path = managed.root.path().join("recovery.json");
    alt_server::operator::create_private(&spec_path, &serde_json::to_vec(&recovery).unwrap())
        .unwrap();
    let restore_args = [
        "cloud-restore",
        "--spec",
        spec_path.to_str().unwrap(),
        "--directory",
        state.to_str().unwrap(),
        "--assets",
        assets.to_str().unwrap(),
        "--database-environment",
        "ALT_OPERATOR_DATABASE",
        "--input",
        input,
        "--identity",
        &identity,
        "--source-fenced",
    ];
    for _ in 0..2 {
        let outcome = managed.ok(&restore_args);
        assert_eq!(outcome["instance"]["runtime_generation"], 20);
        assert_eq!(outcome["manifest_digest"], recovery.backup.manifest_digest);
    }
    let mut db = Client::connect(&managed.dsn, NoTls).unwrap();
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM alt_instance_audit WHERE action='instance.restore'",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        1
    );
    let provision_path = state.join("provision.json");
    let start_args = [
        "cloud-initialize",
        "--spec",
        provision_path.to_str().unwrap(),
        "--directory",
        state.to_str().unwrap(),
        "--assets",
        assets.to_str().unwrap(),
        "--database-environment",
        "ALT_OPERATOR_DATABASE",
    ];
    managed.ok(&start_args);
    let cfg = Config::read(&state.join("config.json")).unwrap();
    let mut recovered = Store::open(
        Client::connect(&managed.dsn, NoTls).unwrap(),
        ByteStore::open(&cfg.bytes_directory).unwrap(),
        cfg.instance.clone(),
        cfg.signing_key().unwrap(),
        limits(),
    )
    .unwrap();
    let runtime = cfg.cloud_runtime().unwrap().unwrap();
    let mut probe = golia_platform_contract::lifecycle::Probe {
        version: 1,
        audience: next_spec.audience.clone(),
        spec_digest: next_spec.digest().unwrap(),
        challenge: "f".repeat(64),
        recovery_manifest_digest: Some(recovery.backup.manifest_digest.clone()),
    };
    runtime.readiness(&mut recovered, probe.clone()).unwrap();
    probe.recovery_manifest_digest = Some("a".repeat(64));
    assert!(runtime.readiness(&mut recovered, probe).is_err());
    db.execute(
        "DELETE FROM alt_instance_audit WHERE action='instance.restore'",
        &[],
    )
    .unwrap();
    assert!(!managed.run(&restore_args).status.success());
    assert!(!managed.run(&start_args).status.success());
}

fn prepare(db: &OperatorDb) -> (Store, String, String) {
    let seed = db.path("receipt.key");
    let admin = db.path("admin.token");
    let password = db.path("password");
    db.ok(&["secret", "receipt-key", "--output", &seed]);
    db.ok(&["secret", "token", "--output", &admin]);
    db.ok(&["initialize"]);
    db.ok(&[
        "bootstrap",
        "owner",
        "--secret",
        &admin,
        "--expires-unix-ms",
        &future().to_string(),
    ]);
    db.ok(&[
        "--credential",
        &admin,
        "repository-create",
        "repository",
        "--quota-bytes",
        "100000000",
    ]);
    std::fs::write(&password, "a private operator test password\n").unwrap();
    std::fs::set_permissions(&password, std::fs::Permissions::from_mode(0o600)).unwrap();
    db.ok(&[
        "--credential",
        &admin,
        "user-create",
        "owner",
        "--login",
        "owner",
        "--display-name",
        "Owner",
        "--password-file",
        &password,
        "--administrator",
    ]);
    let config = Config::read(&db.config).unwrap();
    let mut store = Store::open(
        Client::connect(&db.dsn, NoTls).unwrap(),
        ByteStore::open(&config.bytes_directory).unwrap(),
        config.instance.clone(),
        config.signing_key().unwrap(),
        limits(),
    )
    .unwrap();
    let token = alt_server::operator::read_token(std::path::Path::new(&admin)).unwrap();
    let credential = store.authenticate(&token).unwrap();
    store
        .set_grants(&credential, "repository", "owner", true, true, &grants())
        .unwrap();
    (store, admin, password)
}
fn publish(store: &mut Store, token: &str) -> Payload {
    let mut p = Payload::new("operator-backup-source", None);
    p.publication.key.scope.installation_id = store.spec().installation_id.clone();
    let credential = store.authenticate(token).unwrap();
    store
        .begin(&credential, &p.publication, &p.manifest, 60_000)
        .unwrap();
    let mut sources: Vec<_> = p.contents.iter().map(std::io::Cursor::new).collect();
    let mut inputs: Vec<_> = p
        .manifest
        .entries
        .iter()
        .zip(sources.iter_mut())
        .map(|(entry, source)| alt_server_store::ByteInput {
            digest: &entry.blake3,
            bytes: entry.bytes,
            source,
        })
        .collect();
    let proofs = store
        .persist_contents(&credential, &p.publication.key, &mut inputs)
        .unwrap();
    assert!(proofs.iter().all(|proof| proof.segment_id().is_some()));
    let mapped: std::collections::BTreeMap<_, _> =
        proofs.iter().map(|proof| (proof.blake3(), proof)).collect();
    let entries: Vec<_> = p
        .manifest
        .entries
        .iter()
        .map(|entry| (entry, mapped[entry.blake3.as_str()]))
        .collect();
    store
        .register_contents(&credential, &p.publication.key, &entries)
        .unwrap();
    let work = store
        .acquire_publication(&credential, &p.publication.key, 60_000, 1000)
        .unwrap();
    let graph = alt_repository_service::verify_graph(
        work.source(),
        HashAlgo::Sha1,
        &work.roots().unwrap(),
        alt_repository_service::GraphLimits::from(limits()),
    )
    .unwrap();
    store.mark_ready(&credential, &work, &graph).unwrap();
    store.publish(&credential, &work, &graph).unwrap();
    p
}
fn encryption_key(root: &std::path::Path) -> (String, String) {
    let path = root.join("backup.agekey");
    let output = Command::new("age-keygen")
        .arg("-o")
        .arg(&path)
        .output()
        .expect("age-keygen required by operator acceptance");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = Command::new("age-keygen")
        .arg("-y")
        .arg(&path)
        .output()
        .unwrap();
    assert!(output.status.success());
    (
        path.to_str().unwrap().into(),
        String::from_utf8(output.stdout).unwrap().trim().into(),
    )
}

#[test]
#[ignore = "requires isolated PostgreSQL, matching pg_dump/pg_restore and age tools; exercises encrypted real operator restoration"]
fn encrypted_backup_authenticates_every_entry_and_restores_refs_with_new_epoch() {
    let source = OperatorDb::new();
    let (mut store, admin, password) = prepare(&source);
    let token = alt_server::operator::read_token(std::path::Path::new(&admin)).unwrap();
    let p = publish(&mut store, &token);
    let trust = source.ok(&["health"])["receipt_public_key"]
        .as_str()
        .unwrap()
        .to_owned();
    let offsite = tempfile::tempdir().unwrap();
    let (identity, recipient) = encryption_key(offsite.path());
    let backup = source.path("backup");
    let result = source.ok(&["backup", "--recipient", &recipient, "--output", &backup]);
    assert_eq!(result["backup_created"], true);
    assert_eq!(result["restored"], false);
    assert_eq!(
        result["catalog"]["repositories"][0]["refs"]["refs/heads/main"],
        p.commit.to_string()
    );
    assert!(
        !source
            .run(&["backup", "--recipient", &recipient, "--output", &backup])
            .status
            .success()
    );
    // Save both completed artifacts outside the source instance tree, then use
    // only that independent copy for verification and isolated restoration.
    let external = offsite.path().join("saved-backup");
    std::fs::create_dir(&external).unwrap();
    for name in ["manifest.json", "payload.tar.age"] {
        std::fs::copy(
            std::path::Path::new(&backup).join(name),
            external.join(name),
        )
        .unwrap();
    }
    let input = external.to_str().unwrap();
    let verify = [
        "backup-verify",
        "--input",
        input,
        "--identity",
        &identity,
        "--trusted-receipt-key",
        &trust,
        "--maximum-bytes",
        "100000000",
    ];
    assert_eq!(source.ok(&verify)["verified"], true);
    let mut wrong = verify.to_vec();
    wrong[6] = "0000000000000000000000000000000000000000000000000000000000000000";
    assert!(!source.run(&wrong).status.success());
    let mut small = verify.to_vec();
    small[8] = "1";
    assert!(!source.run(&small).status.success());
    let encrypted = std::fs::read(external.join("payload.tar.age")).unwrap();
    let receipt_secret = std::fs::read(source.path("receipt.key")).unwrap();
    assert!(!encrypted.windows(32).any(|w| w == receipt_secret));
    let mut corrupted = encrypted.clone();
    let last = corrupted.len() - 1;
    corrupted[last] ^= 1;
    std::fs::write(external.join("payload.tar.age"), corrupted).unwrap();
    assert!(!source.run(&verify).status.success());
    std::fs::write(external.join("payload.tar.age"), encrypted).unwrap();
    let target = OperatorDb::new();
    let mut config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&target.config).unwrap()).unwrap();
    config["instance"]["runtime_generation"] = serde_json::json!(10);
    std::fs::write(&target.config, serde_json::to_vec(&config).unwrap()).unwrap();
    let restore = [
        "restore",
        "--input",
        input,
        "--identity",
        &identity,
        "--trusted-receipt-key",
        &trust,
        "--maximum-bytes",
        "100000000",
    ];
    assert!(
        !target.run(&restore).status.success(),
        "external source fencing must be explicit"
    );
    assert!(!target.root.path().join("bytes").exists());
    let mut restore = restore.to_vec();
    restore.push("--source-fenced");
    let restored = target.ok(&restore);
    assert_eq!(restored["restored"], true);
    assert_eq!(restored["old_credentials_revoked"], true);
    assert_eq!(restored["catalog"]["repositories"][0]["recovery_epoch"], 10);
    assert_eq!(
        restored["catalog"]["repositories"][0]["refs"]["refs/heads/main"],
        p.commit.to_string()
    );
    assert!(
        !target.run(&restore).status.success(),
        "cannot overwrite a restored target"
    );
    assert_eq!(target.ok(&["health"])["ready"], true);
    assert!(
        !target
            .run(&[
                "--credential",
                &admin,
                "repository-create",
                "forbidden-old-token",
                "--quota-bytes",
                "10000"
            ])
            .status
            .success()
    );
    target.ok(&[
        "recover-local-administrator",
        "owner",
        "--password-file",
        &password,
        "--reason",
        "isolated backup recovery test",
    ]);
    let current = store.authenticate(&token).unwrap();
    assert_eq!(
        store
            .repository_info(&current, "repository")
            .unwrap()
            .scope
            .recovery_epoch,
        1
    );
    assert_eq!(source.ok(&["health"])["instance"]["runtime_generation"], 1);
}

fn tool_path(name: &str) -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|dir| dir.join(name))
        .find(|path| path.is_file())
        .unwrap_or_else(|| panic!("missing test tool {name}"))
}
struct StoppedTool(libc::pid_t);
impl Drop for StoppedTool {
    fn drop(&mut self) {
        // Exact PID written by our own child, never a process-name search.
        unsafe {
            libc::kill(self.0, libc::SIGKILL);
        }
    }
}
fn interrupted_tool(
    db: &OperatorDb,
    tool: &str,
    after: bool,
    args: &[&str],
) -> (ServerProcess, StoppedTool) {
    let tools = db.root.path().join(format!("stop-{tool}"));
    std::fs::create_dir(&tools).unwrap();
    let marker = tools.join("pid");
    let wrapper = tools.join(tool);
    let script = if after {
        "#!/bin/sh\nset -eu\n\"$ALT_FAULT_REAL_TOOL\" \"$@\"\nprintf '%s\\n' \"$$\" > \"$ALT_FAULT_MARKER\"\nkill -STOP \"$$\"\n"
    } else {
        "#!/bin/sh\nset -eu\nprintf '%s\\n' \"$$\" > \"$ALT_FAULT_MARKER\"\nkill -STOP \"$$\"\nexec \"$ALT_FAULT_REAL_TOOL\" \"$@\"\n"
    };
    let quote =
        |path: &std::path::Path| format!("'{}'", path.to_str().unwrap().replace('\'', "'\\''"));
    let script = script.replacen(
        "set -eu\n",
        &format!(
            "set -eu\nALT_FAULT_REAL_TOOL={}\nALT_FAULT_MARKER={}\n",
            quote(&tool_path(tool)),
            quote(&marker)
        ),
        1,
    );
    std::fs::write(&wrapper, script).unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
    let scratch = tools.join("scratch");
    std::fs::create_dir(&scratch).unwrap();
    let mut paths = vec![tools.clone()];
    paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let mut child = ServerProcess(
        db.command()
            .args(args)
            .env("PATH", std::env::join_paths(paths).unwrap())
            .env("ALT_FAULT_REAL_TOOL", tool_path(tool))
            .env("ALT_FAULT_MARKER", &marker)
            .env("TMPDIR", &scratch)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        if let Ok(pid) = std::fs::read_to_string(&marker)
            && let Ok(pid) = pid.trim().parse::<libc::pid_t>()
        {
            return (child, StoppedTool(pid));
        }
        if let Some(status) = child.0.try_wait().unwrap() {
            let mut error = String::new();
            if let Some(mut pipe) = child.0.stderr.take() {
                std::io::Read::read_to_string(&mut pipe, &mut error).unwrap();
            }
            panic!("operator exited before controlled boundary: {status}: {error}");
        }
        assert!(
            std::time::Instant::now() < deadline,
            "operator never reached controlled boundary"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
#[ignore = "requires isolated PostgreSQL and age; kills owned operator processes at actual backup/restore boundaries"]
fn interrupted_backup_is_incomplete_and_interrupted_restore_cannot_serve_old_credentials() {
    let source = OperatorDb::new();
    let (mut store, admin, _password) = prepare(&source);
    let token = alt_server::operator::read_token(std::path::Path::new(&admin)).unwrap();
    let p = publish(&mut store, &token);
    let trust = source.ok(&["health"])["receipt_public_key"]
        .as_str()
        .unwrap()
        .to_owned();
    let external = tempfile::tempdir().unwrap();
    let (identity, recipient) = encryption_key(external.path());
    let interrupted = source.path("interrupted-backup");
    let (mut process, stopped) = interrupted_tool(
        &source,
        "age",
        false,
        &[
            "backup",
            "--recipient",
            &recipient,
            "--output",
            &interrupted,
        ],
    );
    assert!(
        !std::path::Path::new(&interrupted)
            .join("manifest.json")
            .exists()
    );
    process.0.kill().unwrap();
    process.0.wait().unwrap();
    drop(stopped);
    assert_eq!(
        source.ok(&["health"])["ready"],
        true,
        "snapshot barrier must be released by a real process crash"
    );
    assert!(
        !source
            .run(&[
                "backup-verify",
                "--input",
                &interrupted,
                "--identity",
                &identity,
                "--trusted-receipt-key",
                &trust,
                "--maximum-bytes",
                "100000000"
            ])
            .status
            .success()
    );
    let complete = external.path().join("complete");
    let input = complete.to_str().unwrap();
    source.ok(&["backup", "--recipient", &recipient, "--output", input]);
    let target = OperatorDb::new();
    let mut config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&target.config).unwrap()).unwrap();
    config["instance"]["runtime_generation"] = serde_json::json!(10);
    std::fs::write(&target.config, serde_json::to_vec(&config).unwrap()).unwrap();
    let restore = [
        "restore",
        "--input",
        input,
        "--identity",
        &identity,
        "--trusted-receipt-key",
        &trust,
        "--maximum-bytes",
        "100000000",
        "--source-fenced",
    ];
    let (mut process, stopped) = interrupted_tool(&target, "pg_restore", true, &restore);
    // The SQL commit is real, but activation has not happened. New config and
    // old database generations deliberately disagree throughout this window.
    let mut client = Client::connect(&target.dsn, NoTls).unwrap();
    assert_eq!(
        client
            .query_one("SELECT oid FROM alt_refs WHERE name='refs/heads/main'", &[])
            .unwrap()
            .get::<_, String>(0),
        p.commit.to_string()
    );
    process.0.kill().unwrap();
    process.0.wait().unwrap();
    drop(stopped);
    assert!(!target.run(&["health"]).status.success());
    assert!(!target.run(&["serve"]).status.success());
    assert!(
        !target.run(&restore).status.success(),
        "partial target must not be silently overwritten"
    );
    assert_eq!(
        client
            .query_one("SELECT runtime_generation FROM alt_instance", &[])
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    assert_eq!(source.ok(&["health"])["ready"], true);
}
