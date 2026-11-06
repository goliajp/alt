use alt_remote_protocol::{ActorKind, Attribution, DeploymentMode, Limits};
use alt_repository_service::{Grant, RefSelector};
use alt_server::{HttpOptions, NativeOptions, StoreFactory};
use alt_server_store::{ByteStore, Credential, InstanceSpec, Store};
use ed25519_dalek::SigningKey;
use postgres::{Client, NoTls};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    process::{Command, Output},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

fn limits() -> Limits {
    Limits {
        request_bytes: 8 * 1024 * 1024,
        ref_updates: 100,
        record_roots: 100,
        manifest_entries: 10000,
        graph_objects: 10000,
        graph_edges: 100000,
        structured_bytes: 4 * 1024 * 1024,
        tree_paths: 10000,
        path_bytes: 4096,
        walk_steps: 1_000_000,
        chunk_bytes: 1024 * 1024,
        object_bytes: 256 * 1024 * 1024,
        total_bytes: 512 * 1024 * 1024,
    }
}
fn task_limits() -> alt_task_service::TaskLimits {
    alt_task_service::TaskLimits {
        objects: 10000,
        records: 1000,
        edges: 10000,
        structured_bytes: 4 * 1024 * 1024,
        total_bytes: 512 * 1024 * 1024,
        work_bytes: 4 * 1024 * 1024 * 1024,
        artifact_bytes: 256 * 1024 * 1024,
        paths: 10000,
        path_bytes: 4096,
        walk_steps: 1_000_000,
    }
}
fn future() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
        + 3_600_000
}
struct Server {
    url: String,
    token: String,
    bob: String,
    reviewer: String,
    store: Store,
    admin: Credential,
    admin_dsn: String,
    database: String,
    _root: tempfile::TempDir,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Server {
    fn new() -> Self {
        assert_eq!(
            std::env::var("ALT_SERVER_TEST_ISOLATED").as_deref(),
            Ok("1"),
            "requires isolated PostgreSQL"
        );
        let admin_dsn = std::env::var("ALT_SERVER_TEST_ADMIN_DSN").unwrap();
        let database = format!("alt_cli_remote_{}", std::process::id());
        Client::connect(&admin_dsn, NoTls)
            .unwrap()
            .batch_execute(&format!("CREATE DATABASE {database}"))
            .unwrap();
        let root = tempfile::tempdir().unwrap();
        let spec = InstanceSpec {
            installation_id: "installation".into(),
            workspace_id: "workspace".into(),
            deployment_mode: DeploymentMode::SelfHosted,
            runtime_generation: 1,
        };
        let mut config: postgres::Config = admin_dsn.parse().unwrap();
        config.dbname(&database);
        let bytes = ByteStore::open(&root.path().join("bytes")).unwrap();
        let mut store = Store::initialize(
            config.connect(NoTls).unwrap(),
            bytes.clone(),
            spec.clone(),
            SigningKey::from_bytes(&[7; 32]),
            limits(),
        )
        .unwrap();
        let token = store.bootstrap_admin("alice", future()).unwrap();
        let admin = store.authenticate(&token).unwrap();
        store
            .create_repository(&admin, "repository", 512 * 1024 * 1024)
            .unwrap();
        let attribution = Attribution {
            kind: ActorKind::Human,
            on_behalf_of: None,
            delegation_id: None,
            agent_session: None,
        };
        let bob = store
            .issue_local_token(&admin, "bob", &attribution, future(), false)
            .unwrap();
        let reviewer = store
            .issue_local_token(&admin, "reviewer", &attribution, future(), false)
            .unwrap();
        let grants = vec![Grant {
            refs: RefSelector::Namespace("refs/".into()),
            create: true,
            update: true,
            delete: true,
            force: true,
            paths: None,
        }];
        for actor in ["alice", "bob"] {
            store
                .set_grants(&admin, "repository", actor, true, true, &grants)
                .unwrap();
        }
        store
            .set_grants(&admin, "repository", "reviewer", true, false, &[])
            .unwrap();
        store
            .set_review_grant(&admin, "repository", "reviewer", true)
            .unwrap();
        let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        socket.set_nonblocking(true).unwrap();
        let origin = alt_browser_auth::PublicOrigin::parse(&format!(
            "http://{}",
            socket.local_addr().unwrap()
        ))
        .unwrap();
        store.enable_browser(&admin, &origin).unwrap();
        store
            .create_local_user(
                &admin,
                "reviewer",
                "reviewer",
                "Independent reviewer",
                "a private browser acceptance password",
                false,
            )
            .unwrap();
        let asset_dir = std::env::var_os("ALT_WORKBENCH_ASSETS")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                let assets = root.path().join("web-assets");
                std::fs::create_dir(&assets).unwrap();
                std::fs::write(
                    assets.join("index.html"),
                    "<!doctype html><title>alt acceptance</title>",
                )
                .unwrap();
                assets
            });
        let browser = alt_server::WebOptions {
            origin,
            assets: alt_server::WebAssets::load(&asset_dir).unwrap(),
            session_ttl_ms: 600_000,
        };
        let factory = StoreFactory {
            cloud: None,
            database: config,
            bytes,
            instance: spec,
            signing_key: SigningKey::from_bytes(&[7; 32]),
            limits: limits(),
            connect_timeout: Duration::from_secs(3),
            statement_timeout: Duration::from_secs(10),
        };
        let options = HttpOptions {
            connections: 16,
            workers: 4,
            header_bytes: 16384,
            headers: 32,
            header_timeout: Duration::from_secs(3),
            body_timeout: Duration::from_secs(15),
            work_timeout: Duration::from_secs(30),
            connection_timeout: Duration::from_secs(60),
            native: NativeOptions {
                task_limits: task_limits(),
                upload_ttl_ms: 600_000,
                worker_lease_ms: 60_000,
                approval_ttl_ms: 300_000,
            },
        };
        let (address_tx, address_rx) = std::sync::mpsc::channel();
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        let thread = std::thread::spawn(move || {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap()
                .block_on(async {
                    let listener = tokio::net::TcpListener::from_std(socket).unwrap();
                    address_tx.send(listener.local_addr().unwrap()).unwrap();
                    alt_server::serve_with_browser(
                        listener,
                        factory,
                        options,
                        Some(browser),
                        async {
                            let _ = shutdown_rx.await;
                        },
                    )
                    .await
                    .unwrap();
                });
        });
        Self {
            url: format!("http://{}", address_rx.recv().unwrap()),
            token,
            bob,
            reviewer,
            store,
            admin,
            admin_dsn,
            database,
            _root: root,
            shutdown: Some(shutdown_tx),
            thread: Some(thread),
        }
    }
    fn receipt_key(&self) -> String {
        alt_input::hex(SigningKey::from_bytes(&[7; 32]).verifying_key().as_bytes())
    }
    fn counts(&self) -> (i64, i64) {
        let mut config: postgres::Config = self.admin_dsn.parse().unwrap();
        config.dbname(&self.database);
        let row = config
            .connect(NoTls)
            .unwrap()
            .query_one(
                "SELECT (SELECT count(*) FROM alt_audit),(SELECT count(*) FROM alt_outbox)",
                &[],
            )
            .unwrap();
        (row.get(0), row.get(1))
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.shutdown.take().unwrap().send(());
        self.thread.take().unwrap().join().unwrap();
        Client::connect(&self.admin_dsn, NoTls)
            .unwrap()
            .batch_execute(&format!("DROP DATABASE {} WITH (FORCE)", self.database))
            .unwrap();
    }
}
fn run(root: &Path, token: &str, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_alt"))
        .current_dir(root)
        .args(args)
        .env("ALT_NO_DAEMON", "1")
        .env("ALT_SERVER_TOKEN", token)
        .env("USER", "remote-test")
        .env("GIT_AUTHOR_NAME", "remote-test")
        .env("GIT_AUTHOR_EMAIL", "remote@test")
        .env_remove("ALT_PROTECTED_OVERRIDE")
        .output()
        .unwrap()
}
fn ok(root: &Path, token: &str, args: &[&str]) -> Output {
    let output = run(root, token, args);
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}
fn value(root: &Path, token: &str, args: &[&str]) -> Value {
    serde_json::from_slice(&ok(root, token, args).stdout).unwrap()
}
fn git(root: &Path, token: &str, args: &[&str]) -> Vec<u8> {
    let output = Command::new("git")
        .current_dir(root)
        .args(["-c", "protocol.version=2"])
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "http.extraHeader")
        .env(
            "GIT_CONFIG_VALUE_0",
            format!("Authorization: Bearer {token}"),
        )
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}
fn connect(root: &Path, server: &Server, token: &str, name: &str) {
    value(
        root,
        token,
        &[
            "task",
            "remote",
            "connect",
            name,
            "--url",
            &server.url,
            "--repository",
            "repository",
            "--receipt-key",
            &server.receipt_key(),
            "--allow-loopback-http",
            "--json",
        ],
    );
}
fn preview(root: &Path, token: &str, name: &str, plan: &Value, path: &Path) -> Value {
    std::fs::write(path, serde_json::to_vec(plan).unwrap()).unwrap();
    value(
        root,
        token,
        &[
            "task",
            "remote",
            "preview",
            name,
            "--plan",
            path.to_str().unwrap(),
            "--json",
        ],
    )
}
fn string<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap()
}

#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP, real Git and disk-backed runner; exercised by closing gate"]
fn two_clients_continue_original_task_revalidate_review_publish_and_export_without_forged_history()
{
    use std::os::unix::fs::PermissionsExt;
    let mut server = Server::new();
    std::fs::create_dir_all(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let root = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let scratch = tempfile::tempdir_in(std::env::var_os("HOME").unwrap()).unwrap();
    let alice = root.path().join("alice");
    let bob = root.path().join("bob");
    let aw = root.path().join("alice-work");
    let bw = root.path().join("bob-work");
    std::fs::create_dir(&alice).unwrap();
    ok(&alice, &server.token, &["init", "."]);
    std::fs::write(alice.join("source.txt"), "base\n").unwrap();
    std::fs::write(
        alice.join("gate.sh"),
        "test -s source.txt\nprintf verified\n",
    )
    .unwrap();
    ok(&alice, &server.token, &["add", "."]);
    ok(&alice, &server.token, &["commit", "-m", "base"]);
    let baseline_git = root.path().join("baseline-git");
    ok(
        &alice,
        &server.token,
        &["export", baseline_git.to_str().unwrap()],
    );
    let url = format!("{}/git/repository", server.url);
    git(
        &baseline_git,
        &server.token,
        &[
            "push",
            "--atomic",
            &url,
            "refs/heads/main",
            "refs/notes/alt/meta",
        ],
    );
    let bob_git = root.path().join("bob-git");
    git(
        root.path(),
        &server.bob,
        &["clone", &url, bob_git.to_str().unwrap()],
    );
    git(
        &bob_git,
        &server.bob,
        &["fetch", "origin", "refs/notes/alt/meta:refs/notes/alt/meta"],
    );
    ok(&bob_git, &server.bob, &["import", bob.to_str().unwrap()]);
    ok(
        &alice,
        &server.token,
        &["workspace", "add", "work", aw.to_str().unwrap()],
    );
    ok(
        &bob,
        &server.bob,
        &["workspace", "add", "work", bw.to_str().unwrap()],
    );
    let tools = json!([{"name":"bash","path":"/bin/bash","version_args":["--version"]}]);
    let environment = json!({"PATH":"/usr/bin:/bin"});
    let probe = value(
        scratch.path(),
        &server.token,
        &[
            "runner",
            "probe",
            "--bindings",
            &tools.to_string(),
            "--environment",
            &environment.to_string(),
            "--json",
        ],
    );
    let definition = json!({"purpose":"continue across two clients","authority":"owner","allowed_paths":["source.txt"],"auxiliary":[],"exclusions":[],"validation":{"commands":[["bash","gate.sh"]],"cwd":".","environment":{"values":environment,"complete":true},"tools":probe["tools"],"external":[],"validation":{"outputs":[]}}});
    let task = value(
        &aw,
        &server.token,
        &[
            "task",
            "create",
            "--definition",
            &definition.to_string(),
            "--target",
            "refs/heads/main",
            "--json",
        ],
    );
    let task_id = string(&task, "task_id");
    std::fs::write(aw.join("source.txt"), "first-client\n").unwrap();
    ok(&aw, &server.token, &["add", "source.txt"]);
    let candidate = value(
        &aw,
        &server.token,
        &["candidate", "create", task_id, "--json"],
    );
    let profile = json!({"kind":"local","scratch":scratch.path(),"tools":tools,"timeout_ms":5000})
        .to_string();
    let evidence = value(
        &aw,
        &server.token,
        &[
            "evidence",
            "run",
            string(&candidate, "candidate_id"),
            "--profile",
            &profile,
            "--json",
        ],
    );
    assert_eq!(evidence["validity"]["eligible"], true);
    connect(&aw, &server, &server.token, "origin");
    connect(&bw, &server, &server.bob, "origin");
    let plan = json!({"selection":{"roots":[{"kind":"evidence","id":evidence["evidence_id"]}],"companions":[{"kind":"task-baseline","id":task["baseline_id"]}],"omissions":[],"attestations":[]},"refs":[]});
    let first = preview(
        &aw,
        &server.token,
        "origin",
        &plan,
        &root.path().join("first-plan.json"),
    );
    let first_id = string(&first, "publication_id");
    let first_digest = string(&first, "request_digest");
    let published = value(
        &aw,
        &server.token,
        &["task", "remote", "publish", first_id, "--json"],
    );
    assert_eq!(published["state"], "committed");
    let counts = server.counts();
    assert_eq!(
        value(
            &aw,
            &server.token,
            &["task", "remote", "publish", first_id, "--json"]
        )["receipt"],
        published["receipt"]
    );
    assert_eq!(server.counts(), counts);
    let original_request = task["task"]["creator_request"].as_str().unwrap();
    let received = value(
        &bw,
        &server.bob,
        &[
            "task",
            "remote",
            "receive",
            "origin",
            "--kind",
            "evidence",
            "--id",
            string(&evidence, "evidence_id"),
            "--publication",
            first_digest,
            "--json",
        ],
    );
    assert_eq!(received["execution"], "none");
    assert!(!bob.join(".alt/requests").join(original_request).exists());
    let before_adopt = run(
        &bw,
        &server.bob,
        &["candidate", "create", task_id, "--json"],
    );
    assert!(!before_adopt.status.success());
    value(
        &bw,
        &server.bob,
        &[
            "task",
            "remote",
            "adopt",
            task_id,
            "--publication",
            first_digest,
            "--json",
        ],
    );
    let original_record = std::fs::read(
        alice
            .join(".alt/task-data/records/task")
            .join(format!("{task_id}.json")),
    )
    .unwrap();
    assert_eq!(
        std::fs::read(
            bob.join(".alt/task-data/records/task")
                .join(format!("{task_id}.json"))
        )
        .unwrap(),
        original_record
    );
    std::fs::write(bw.join("source.txt"), "second-client\n").unwrap();
    ok(&bw, &server.bob, &["add", "source.txt"]);
    let continued = value(
        &bw,
        &server.bob,
        &[
            "candidate",
            "create",
            task_id,
            "--parent",
            string(&candidate, "candidate_id"),
            "--json",
        ],
    );
    let current = value(
        &bw,
        &server.bob,
        &[
            "evidence",
            "run",
            string(&continued, "candidate_id"),
            "--profile",
            &profile,
            "--json",
        ],
    );
    assert_eq!(current["validity"]["eligible"], true);
    let keyfile = root.path().join("collector.key");
    std::fs::write(&keyfile, [9u8; 32]).unwrap();
    std::fs::set_permissions(&keyfile, std::fs::Permissions::from_mode(0o600)).unwrap();
    let attestation = value(
        &bw,
        &server.bob,
        &[
            "task",
            "remote",
            "attest",
            string(&current, "evidence_id"),
            "--key-file",
            keyfile.to_str().unwrap(),
            "--json",
        ],
    );
    server
        .store
        .set_collector_policy(
            &server.admin,
            "repository",
            &alt_task_service::CollectorPolicy {
                keys: BTreeMap::from([(
                    string(&attestation["attestation"], "signing_key_id").into(),
                    alt_task_service::TrustedCollector {
                        public_key_hex: string(&attestation, "public_key").into(),
                        runners: BTreeSet::from([string(&current["evidence"], "runner").into()]),
                        expires_unix_ms: future(),
                    },
                )]),
            },
        )
        .unwrap();
    let reviewed = value(
        &bw,
        &server.bob,
        &[
            "task",
            "review",
            string(&continued, "candidate_id"),
            "--evidence",
            string(&current, "evidence_id"),
            "--reviewer",
            "bob",
            "--reason",
            "inspected complete second-client change",
            "-m",
            "fix: continue original task",
            "--json",
        ],
    );
    ok(&bob, &server.bob, &["switch", "-c", "hold"]);
    let integrated = value(
        &bw,
        &server.bob,
        &[
            "task",
            "integrate",
            string(&reviewed, "review_id"),
            "--json",
        ],
    );
    let export = root.path().join("delivery");
    let delivered = value(
        &bw,
        &server.bob,
        &[
            "task",
            "export",
            string(&integrated, "integration_id"),
            export.to_str().unwrap(),
            "--git",
            "/usr/bin/git",
            "--json",
        ],
    );
    assert_eq!(
        git(&export, &server.bob, &["show", "main:source.txt"]),
        b"second-client\n"
    );
    let integration = &integrated["integration"];
    let final_plan = json!({"selection":{"roots":[{"kind":"delivery","id":delivered["delivery_id"]}],"companions":[],"omissions":[],"attestations":[attestation["attestation"]]},"refs":[{"name":integration["target"],"expected":integration["before"],"new":integration["commit"],"force":false},{"name":"refs/notes/alt/meta","expected":integration["notes_before"],"new":integration["notes_after"],"force":false}]});
    let final_preview = preview(
        &bw,
        &server.bob,
        "origin",
        &final_plan,
        &root.path().join("final-plan.json"),
    );
    let final_id = string(&final_preview, "publication_id");
    let final_digest = string(&final_preview, "request_digest");
    assert_eq!(
        value(
            &bw,
            &server.bob,
            &[
                "task",
                "remote",
                "publish",
                final_id,
                "--prepare-only",
                "--json"
            ]
        )["state"],
        "ready"
    );
    let web_base = format!("/web/repositories/repository/epochs/1/requests/bob/{final_id}");
    let (cookie, browser_session) = browser_login(&server);
    let (status, browser_review) =
        browser_json(&server, &format!("{web_base}/review"), &cookie, None, None);
    assert_eq!(status, 200, "{browser_review}");
    assert_eq!(browser_review["candidate_id"], continued["candidate_id"]);
    assert_eq!(browser_review["historical_reviewer"], "bob");
    assert_eq!(browser_review["eligible"], true, "{browser_review}");
    assert!(
        browser_review["changes"][0]["patch"]
            .as_str()
            .unwrap()
            .contains("+second-client")
    );
    assert_eq!(browser_review["evidence"][0]["accepted"], true);
    let mut stale = browser_review["inspected"].clone();
    stale["request_digest"] = json!("0".repeat(64));
    assert_eq!(
        browser_json(
            &server,
            &format!("{web_base}/approve"),
            &cookie,
            browser_session["csrf"].as_str(),
            Some(json!({"integration_id":integrated["integration_id"],"inspected":stale}))
        )
        .0,
        409
    );
    // Optional real-browser acceptance consumes this same live task and service.
    if let Some(script) = std::env::var_os("ALT_WORKBENCH_ACCEPTANCE") {
        let result = Command::new("node")
            .arg(script)
            .env("ALT_BROWSER_ORIGIN", &server.url)
            .env("ALT_BROWSER_REVIEW", &web_base)
            .env("ALT_BROWSER_TASK", task_id)
            .status()
            .unwrap();
        assert!(result.success(), "real workbench acceptance failed");
    }
    assert_eq!(browser_json(&server, &format!("{web_base}/approve"), &cookie, browser_session["csrf"].as_str(), Some(json!({"integration_id":integrated["integration_id"],"inspected":browser_review["inspected"]}))).0, 200);
    connect(&aw, &server, &server.reviewer, "reviewer");
    let inspected = value(
        &aw,
        &server.reviewer,
        &[
            "task",
            "remote",
            "review",
            "reviewer",
            "--actor",
            "bob",
            "--request",
            final_id,
            "--json",
        ],
    );
    assert_eq!(inspected["request_digest"], final_digest);
    assert_eq!(inspected["reviews"][0]["review_id"], reviewed["review_id"]);
    assert_eq!(
        inspected["reviews"][0]["changes"]["changes"][0]["after"]["utf8"],
        "second-client\n"
    );
    value(
        &aw,
        &server.reviewer,
        &[
            "task",
            "remote",
            "approve",
            "reviewer",
            "--actor",
            "bob",
            "--request",
            final_id,
            "--integration",
            string(&integrated, "integration_id"),
            "--digest",
            final_digest,
            "--json",
        ],
    );
    let committed = value(
        &bw,
        &server.bob,
        &["task", "remote", "publish", final_id, "--json"],
    );
    assert_eq!(committed["state"], "committed");
    assert_eq!(
        value(
            &bw,
            &server.bob,
            &["task", "remote", "status", final_id, "--json"]
        )["observation"]["result"]["receipt"],
        committed["receipt"]
    );
    let (status, task_page) = browser_json(
        &server,
        &format!("/web/repositories/repository/records/task/{task_id}"),
        &cookie,
        None,
        None,
    );
    assert_eq!(status, 200, "{task_page}");
    assert_eq!(task_page["summary"]["id"], task_id);
    assert!(
        task_page["attachments"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["name"] == "original-record.json")
    );
    let (status, deliveries) = browser_json(
        &server,
        "/web/repositories/repository/records/delivery",
        &cookie,
        None,
        None,
    );
    assert_eq!(status, 200, "{deliveries}");
    assert_eq!(deliveries["items"][0]["id"], delivered["delivery_id"]);
    let final_git = root.path().join("final-git");
    git(
        root.path(),
        &server.token,
        &["clone", &url, final_git.to_str().unwrap()],
    );
    assert_eq!(
        git(&final_git, &server.token, &["rev-parse", "HEAD"]),
        git(&export, &server.token, &["rev-parse", "main"])
    );
    assert_eq!(
        git(&final_git, &server.token, &["show", "HEAD:source.txt"]),
        b"second-client\n"
    );
    value(
        &aw,
        &server.token,
        &[
            "task",
            "remote",
            "receive",
            "origin",
            "--kind",
            "delivery",
            "--id",
            string(&delivered, "delivery_id"),
            "--publication",
            final_digest,
            "--json",
        ],
    );
    assert_eq!(
        std::fs::read(aw.join("source.txt")).unwrap(),
        b"first-client\n"
    );
    let b_request = string(integration, "creator_request");
    assert!(!alice.join(".alt/requests").join(b_request).exists());
    assert_eq!(
        value(&aw, &server.token, &["task", "show", task_id, "--json"])["state"],
        "integrated"
    );
    assert_eq!(
        std::fs::read(
            bob.join(".alt/task-data/records/task")
                .join(format!("{task_id}.json"))
        )
        .unwrap(),
        original_record
    );
}

fn browser_request(
    server: &Server,
    path: &str,
    cookie: Option<&str>,
    csrf: Option<&str>,
    body: Option<Value>,
) -> (u16, String, Value) {
    use std::io::Write;
    let headers = tempfile::NamedTempFile::new().unwrap();
    let output = tempfile::NamedTempFile::new().unwrap();
    let mut command = Command::new("curl");
    command
        .args([
            "--silent",
            "--show-error",
            "--max-time",
            "60",
            "--dump-header",
        ])
        .arg(headers.path())
        .arg("--output")
        .arg(output.path())
        .args(["--write-out", "%{http_code}", "--header"])
        .arg(format!("Origin: {}", server.url));
    if let Some(cookie) = cookie {
        command.arg("--header").arg(format!("Cookie: {cookie}"));
    }
    if let Some(csrf) = csrf {
        command.arg("--header").arg(format!("X-Alt-CSRF: {csrf}"));
    }
    if body.is_some() {
        command.args([
            "--header",
            "Content-Type: application/json",
            "--data-binary",
            "@-",
        ]);
    }
    let mut child = command
        .arg(format!("{}{path}", server.url))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(body) = body {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(&serde_json::to_vec(&body).unwrap())
            .unwrap();
    } else {
        drop(child.stdin.take());
    }
    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "browser request failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    (
        std::str::from_utf8(&result.stdout)
            .unwrap()
            .parse()
            .unwrap(),
        std::fs::read_to_string(headers.path()).unwrap(),
        serde_json::from_reader(std::fs::File::open(output.path()).unwrap()).unwrap(),
    )
}
fn browser_login(server: &Server) -> (String, Value) {
    let (status, headers, value) = browser_request(
        server,
        "/web/login",
        None,
        None,
        Some(json!({"login_name":"reviewer","password":"a private browser acceptance password"})),
    );
    assert_eq!(status, 200, "{value}");
    let cookie = headers
        .lines()
        .find_map(|line| {
            line.split_once(':')
                .filter(|(name, _)| name.eq_ignore_ascii_case("set-cookie"))
                .map(|(_, value)| value.trim().split(';').next().unwrap().to_owned())
        })
        .unwrap();
    (cookie, value)
}
fn browser_json(
    server: &Server,
    path: &str,
    cookie: &str,
    csrf: Option<&str>,
    body: Option<Value>,
) -> (u16, Value) {
    let (status, _, value) = browser_request(server, path, Some(cookie), csrf, body);
    (status, value)
}
