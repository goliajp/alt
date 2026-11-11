mod change_links;
mod maintenance;
mod observability;
mod provision;
use super::*;
use alt_server::operator::{Config, HttpBounds};
use std::{
    io::{BufRead, BufReader},
    path::PathBuf,
    process::{Command, Output, Stdio},
};

struct ServerProcess(std::process::Child);
impl Drop for ServerProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct OperatorDb {
    admin: String,
    name: String,
    root: alt_os::temp::TempDir,
    config: PathBuf,
    dsn: String,
}
impl OperatorDb {
    fn new() -> Self {
        assert_eq!(
            std::env::var("ALT_SERVER_TEST_ISOLATED").as_deref(),
            Ok("1")
        );
        let admin = std::env::var("ALT_SERVER_TEST_ADMIN_DSN").unwrap();
        assert!(
            admin.contains("dbname=postgres"),
            "use the isolated PostgreSQL wrapper keyword DSN"
        );
        let name = format!(
            "alt_operator_{}_{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        );
        Client::connect(&admin, NoTls)
            .unwrap()
            .batch_execute(&format!("CREATE DATABASE {name}"))
            .unwrap();
        let dsn = admin.replace("dbname=postgres", &format!("dbname={name}"));
        let root = alt_os::temp::tempdir().unwrap();
        let config = root.path().join("server.json");
        let spec = Config {
            change_links: Vec::new(),
            portable: None,
            cloud: None,
            browser: None,
            version: 1,
            instance: InstanceSpec {
                installation_id: "operator-instance".into(),
                workspace_id: "operator-workspace".into(),
                deployment_mode: DeploymentMode::SelfHosted,
                runtime_generation: 1,
            },
            database_environment: "ALT_OPERATOR_DATABASE".into(),
            bytes_directory: root.path().join("bytes"),
            receipt_key_file: root.path().join("receipt.key"),
            listen: "127.0.0.1:0".parse().unwrap(),
            limits: limits(),
            tasks: task_limits(),
            http: HttpBounds {
                connections: 8,
                workers: 2,
                header_bytes: 16384,
                headers: 32,
                header_timeout_ms: 3000,
                body_timeout_ms: 5000,
                work_timeout_ms: 10000,
                connection_timeout_ms: 30000,
                database_connect_timeout_ms: 3000,
                database_statement_timeout_ms: 5000,
                upload_ttl_ms: 60000,
                worker_lease_ms: 30000,
                approval_ttl_ms: 60000,
            },
        };
        std::fs::write(&config, serde_json::to_vec(&spec).unwrap()).unwrap();
        Self {
            admin,
            name,
            root,
            config,
            dsn,
        }
    }
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_alt-server"));
        command
            .arg("--config")
            .arg(&self.config)
            .env("ALT_OPERATOR_DATABASE", &self.dsn);
        command
    }
    fn run(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }
    fn ok(&self, args: &[&str]) -> serde_json::Value {
        let result = self.run(args);
        assert!(
            result.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        serde_json::from_slice(&result.stdout).unwrap()
    }
    fn path(&self, name: &str) -> String {
        self.root.path().join(name).to_str().unwrap().into()
    }
}
impl Drop for OperatorDb {
    fn drop(&mut self) {
        Client::connect(&self.admin, NoTls)
            .unwrap()
            .batch_execute(&format!("DROP DATABASE {} WITH (FORCE)", self.name))
            .unwrap();
    }
}

#[test]
#[ignore = "requires isolated PostgreSQL and a real operator/server subprocess; exercised by closing gate"]
fn persisted_operator_secrets_resume_without_bootstrap_mode_or_volume_fallback() {
    let db = OperatorDb::new();
    let seed = db.path("receipt.key");
    let admin = db.path("admin.token");
    let bob = db.path("bob.token");
    let extra = db.path("extra.token");
    let expires = future().to_string();
    db.ok(&["secret", "receipt-key", "--output", &seed]);
    db.ok(&["secret", "token", "--output", &admin]);
    let saved = std::fs::read(&seed).unwrap();
    assert_eq!(saved.len(), 32);
    assert!(
        !db.run(&["secret", "receipt-key", "--output", &seed])
            .status
            .success()
    );
    assert_eq!(std::fs::read(&seed).unwrap(), saved);
    assert!(!db.run(&["health"]).status.success());
    assert_eq!(db.ok(&["initialize"])["initialized"], true);
    assert!(!db.run(&["initialize"]).status.success());
    db.ok(&["initialize", "--resume"]);
    db.ok(&[
        "bootstrap",
        "owner",
        "--secret",
        &admin,
        "--expires-unix-ms",
        &expires,
    ]);
    db.ok(&[
        "bootstrap",
        "owner",
        "--secret",
        &admin,
        "--expires-unix-ms",
        &expires,
    ]);
    assert!(
        !db.run(&[
            "bootstrap",
            "different",
            "--secret",
            &admin,
            "--expires-unix-ms",
            &expires
        ])
        .status
        .success()
    );
    db.ok(&["secret", "token", "--output", &extra]);
    assert!(
        !db.run(&[
            "bootstrap",
            "owner",
            "--secret",
            &extra,
            "--expires-unix-ms",
            &expires
        ])
        .status
        .success()
    );
    let health = db.ok(&["health"]);
    assert_eq!(health["ready"], true);
    assert_eq!(health["instance"]["deployment_mode"], "self_hosted");
    db.ok(&[
        "--credential",
        &admin,
        "repository-create",
        "repository",
        "--quota-bytes",
        "100000000",
    ]);
    db.ok(&["secret", "token", "--output", &bob]);
    let issue = [
        "--credential",
        &admin,
        "token-issue",
        "bob",
        "--secret",
        &bob,
        "--expires-unix-ms",
        &expires,
    ];
    db.ok(&issue);
    db.ok(&issue);
    let token = alt_server::operator::read_token(std::path::Path::new(&bob)).unwrap();
    let mut child = ServerProcess(
        db.command()
            .arg("serve")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let stdout = child.0.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut line = String::new();
        BufReader::new(stdout).read_line(&mut line).unwrap();
        tx.send(line).unwrap();
    });
    let line = rx.recv_timeout(Duration::from_secs(15));
    if line.is_err() {
        let _ = child.0.kill();
        let _ = child.0.wait();
        panic!("operator server did not announce readiness");
    }
    reader.join().unwrap();
    let started: serde_json::Value = serde_json::from_str(&line.unwrap()).unwrap();
    let url = format!("http://{}", started["address"].as_str().unwrap());
    let response = ureq::get(&format!("{url}/healthz"))
        .timeout(Duration::from_secs(5))
        .call()
        .unwrap();
    assert_eq!(response.status(), 200);
    let response = ureq::get(&format!("{url}/v1/capabilities"))
        .set("Authorization", &format!("Bearer {token}"))
        .timeout(Duration::from_secs(5))
        .call()
        .unwrap();
    let capabilities: serde_json::Value = serde_json::from_reader(response.into_reader()).unwrap();
    assert_eq!(
        capabilities["receipt_public_key"],
        health["receipt_public_key"]
    );
    child.0.kill().unwrap();
    child.0.wait().unwrap();
    db.ok(&["--credential", &admin, "token-revoke", "--secret", &bob]);
    assert!(!db.run(&issue).status.success());
    let missing = db
        .command()
        .arg("health")
        .env_remove("ALT_OPERATOR_DATABASE")
        .output()
        .unwrap();
    assert!(!missing.status.success());
    let original = std::fs::read(&db.config).unwrap();
    let mut config: serde_json::Value = serde_json::from_slice(&original).unwrap();
    config["instance"]["deployment_mode"] = serde_json::json!("cloud");
    std::fs::write(&db.config, serde_json::to_vec(&config).unwrap()).unwrap();
    for args in [
        &["health"][..],
        &["initialize", "--resume"][..],
        &[
            "bootstrap",
            "owner",
            "--secret",
            &admin,
            "--expires-unix-ms",
            &expires,
        ][..],
    ] {
        assert!(!db.run(args).status.success());
    }
    config = serde_json::from_slice(&original).unwrap();
    config["bytes_directory"] = serde_json::json!(db.path("other-volume"));
    std::fs::write(&db.config, serde_json::to_vec(&config).unwrap()).unwrap();
    assert!(!db.run(&["health"]).status.success());
    assert!(!db.root.path().join("other-volume").exists());
    config = serde_json::from_slice(&original).unwrap();
    config["listen"] = serde_json::json!("0.0.0.0:8080");
    std::fs::write(&db.config, serde_json::to_vec(&config).unwrap()).unwrap();
    assert!(!db.run(&["serve"]).status.success());
    std::fs::write(&db.config, original).unwrap();
    assert_eq!(db.ok(&["health"])["ready"], true);
    let errors = String::from_utf8_lossy(&missing.stderr);
    assert!(!errors.contains(&token));
    assert!(!errors.contains(&db.dsn));
}
