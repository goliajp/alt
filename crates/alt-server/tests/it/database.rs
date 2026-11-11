use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_remote_protocol::*;
use alt_repository_service::{Grant, RefSelector};
use alt_server::{HttpOptions, NativeOptions, StoreFactory};
mod browser;
mod change_links;
mod chunks;
mod cloud;
mod connections;
mod git;
mod native_client;
mod operator;
mod portable;
mod single_copy;
use alt_crypto::ed25519::SigningKey;
use alt_server_store::{ByteStore, Credential, InstanceSpec, Store};
use postgres::{Client, NoTls};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

fn task_limits() -> alt_task_service::TaskLimits {
    alt_task_service::TaskLimits {
        objects: 1000,
        records: 100,
        edges: 1000,
        structured_bytes: 1_000_000,
        total_bytes: 100_000_000,
        work_bytes: 1_000_000_000,
        artifact_bytes: 10_000_000,
        paths: 1000,
        path_bytes: 4096,
        walk_steps: 100_000,
    }
}
struct Running {
    url: String,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Running {
    fn start(f: &Fixture) -> Self {
        Self::start_with_browser(f, None)
    }
    fn start_with_browser(f: &Fixture, browser: Option<alt_server::WebOptions>) -> Self {
        Self::start_with_cloud(f, browser, None)
    }
    fn start_with_cloud(
        f: &Fixture,
        browser: Option<alt_server::WebOptions>,
        cloud: Option<std::sync::Arc<alt_server::cloud::Runtime>>,
    ) -> Self {
        Self::start_with_links(f, browser, cloud, vec![])
    }
    fn start_with_links(
        f: &Fixture,
        browser: Option<alt_server::WebOptions>,
        cloud: Option<std::sync::Arc<alt_server::cloud::Runtime>>,
        links: Vec<alt_server::change_links::ChannelRuntime>,
    ) -> Self {
        Self::start_with_shutdown(f, browser, cloud, links, false)
    }
    fn parts(
        f: &Fixture,
        cloud: Option<std::sync::Arc<alt_server::cloud::Runtime>>,
        links: Vec<alt_server::change_links::ChannelRuntime>,
    ) -> (StoreFactory, HttpOptions) {
        let mut database: postgres::Config = f.admin_dsn.parse().unwrap();
        database.dbname(&f.database);
        let signing_key = SigningKey::from_bytes(&[f.key_seed; 32]);
        let factory = StoreFactory {
            change_links: links.into(),
            cloud,
            database,
            bytes: ByteStore::open(&f.root.path().join("bytes")).unwrap(),
            instance: f.spec.clone(),
            signing_key,
            limits: limits(),
            connect_timeout: Duration::from_secs(3),
            statement_timeout: Duration::from_secs(5),
        };
        let options = HttpOptions {
            connections: 16,
            workers: 4,
            header_bytes: 16384,
            headers: 32,
            header_timeout: Duration::from_secs(3),
            body_timeout: Duration::from_secs(3),
            work_timeout: Duration::from_secs(10),
            connection_timeout: Duration::from_secs(30),
            native: NativeOptions {
                portable: Some(portable::bounds()),
                task_limits: task_limits(),
                upload_ttl_ms: 60_000,
                worker_lease_ms: 30_000,
                approval_ttl_ms: 60_000,
            },
        };
        (factory, options)
    }
    /// what the service reports when it refuses to start with this browser configuration
    fn refusal(f: &Fixture, browser: Option<alt_server::WebOptions>) -> String {
        let (factory, options) = Self::parts(f, None, vec![]);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            alt_server::serve_with_browser(listener, factory, options, browser, async {})
                .await
                .unwrap_err()
                .to_string()
        })
    }
    fn start_with_shutdown(
        f: &Fixture,
        browser: Option<alt_server::WebOptions>,
        cloud: Option<std::sync::Arc<alt_server::cloud::Runtime>>,
        links: Vec<alt_server::change_links::ChannelRuntime>,
        abort: bool,
    ) -> Self {
        let (factory, options) = Self::parts(f, cloud, links);
        let (address_tx, address_rx) = std::sync::mpsc::channel();
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        let thread = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                address_tx.send(listener.local_addr().unwrap()).unwrap();
                if abort {
                    let serving = tokio::spawn(alt_server::serve_with_browser(
                        listener,
                        factory,
                        options,
                        browser,
                        std::future::pending(),
                    ));
                    let _ = shutdown_rx.await;
                    serving.abort();
                    assert!(serving.await.unwrap_err().is_cancelled());
                } else {
                    alt_server::serve_with_browser(listener, factory, options, browser, async {
                        let _ = shutdown_rx.await;
                    })
                    .await
                    .unwrap();
                }
            });
        });
        let address = address_rx.recv().unwrap();
        Self {
            url: format!("http://{address}"),
            shutdown: Some(shutdown_tx),
            thread: Some(thread),
        }
    }
    fn send(&self, method: &str, path: &str, token: Option<&str>, body: &[u8]) -> ureq::Response {
        let mut request =
            ureq::request(method, &format!("{}{path}", self.url)).timeout(Duration::from_secs(15));
        if method == "POST"
            && (path.ends_with("/requests")
                || path.ends_with("/approve")
                || path.contains("/changes/")
                || path == "/api/v1/change-links/inbox")
        {
            request = request.set("Content-Type", "application/json");
        }
        if let Some(token) = token {
            request = request.set("Authorization", &format!("Bearer {token}"));
        }
        match request.send_bytes(body) {
            Ok(response) => response,
            Err(ureq::Error::Status(_, response)) => response,
            Err(error) => panic!("HTTP transport failed: {error}"),
        }
    }
    fn json(
        &self,
        method: &str,
        path: &str,
        token: &str,
        body: &impl serde::Serialize,
    ) -> (u16, serde_json::Value) {
        let response = self.send(
            method,
            path,
            Some(token),
            &serde_json::to_vec(body).unwrap(),
        );
        let status = response.status();
        (
            status,
            serde_json::from_reader(response.into_reader()).unwrap(),
        )
    }
    fn metrics(&self, token: &str) -> String {
        let response = self.send("GET", "/v1/instance/metrics", Some(token), b"");
        assert_eq!(response.status(), 200);
        response.into_string().unwrap()
    }
    fn wait_metric(&self, token: &str, sample: &str) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let text = self.metrics(token);
            if text.lines().any(|line| line == sample) {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "missing metric {sample}: {text}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    fn command(&self, method: &str, path: &str, token: &str) -> (u16, serde_json::Value) {
        let response = self.send(method, path, Some(token), b"");
        let status = response.status();
        (
            status,
            serde_json::from_reader(response.into_reader()).unwrap(),
        )
    }
}
impl Drop for Running {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        self.thread.take().unwrap().join().unwrap();
    }
}

#[test]
#[ignore = "requires explicitly isolated PostgreSQL and loopback HTTP; exercised by server acceptance and closing gate"]
fn http_rejects_wrong_json_media_type_chunked_overflow_and_body_trailers() {
    use std::io::{Read, Write};
    let f = Fixture::new();
    let server = Running::start(&f);
    let address = server.url.strip_prefix("http://").unwrap();
    let send = |request: &str| {
        let mut socket = std::net::TcpStream::connect(address).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        socket.write_all(request.as_bytes()).unwrap();
        let mut response = String::new();
        socket.read_to_string(&mut response).unwrap();
        response
    };
    let response = send(&format!(
        "POST /v1/repositories/repository/epochs/1/requests HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {}\r\nContent-Type: text/plain\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}",
        f.token
    ));
    assert!(response.starts_with("HTTP/1.1 415"), "{response}");
    let response = send(&format!(
        "GET /v1/capabilities HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {}\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n1\r\nx\r\n0\r\n\r\n",
        f.token
    ));
    assert!(response.starts_with("HTTP/1.1 413"), "{response}");
    let response = send(&format!(
        "GET /v1/capabilities HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {}\r\nTransfer-Encoding: chunked\r\nTrailer: X-Unexpected\r\nConnection: close\r\n\r\n0\r\nX-Unexpected: value\r\n\r\n",
        f.token
    ));
    assert!(response.starts_with("HTTP/1.1 400"), "{response}");
    assert_eq!(f.counts(), (0, 0, 0));
}

#[test]
#[ignore = "requires explicitly isolated PostgreSQL and loopback HTTP; exercised by server acceptance and closing gate"]
fn http_publication_upload_prepare_release_commit_replay_restart_and_revoke() {
    let mut f = Fixture::new();
    let server = Running::start(&f);
    assert_eq!(
        server.send("GET", "/v1/capabilities", None, b"").status(),
        401
    );
    let (status, caps) = server.command("GET", "/v1/capabilities", &f.token);
    assert_eq!(status, 200, "{caps}");
    assert_eq!(caps["capabilities"]["deployment_mode"], "self_hosted");
    let p = Payload::new("native-http", None);
    let base = "/v1/repositories/repository/epochs/1/requests";
    let request = format!("{base}/owner/native-http");
    let begin = alt_server::BeginRequest {
        publication: p.publication.clone(),
        manifest: p.manifest.clone(),
    };
    let (status, state) = server.json("POST", base, &f.token, &begin);
    assert_eq!(status, 200, "{state}");
    assert_eq!(state["state"], "receiving");
    let entry = &p.manifest.entries[1];
    let content = format!(
        "/v1/repositories/repository/epochs/1/content/{}",
        entry.blake3
    );
    assert_eq!(
        server.send("GET", &content, Some(&f.token), b"").status(),
        403,
        "unpublished bytes must remain private"
    );
    let wrong = server.send(
        "PUT",
        &format!("{request}/content/{}", entry.blake3),
        Some(&f.token),
        b"wrong bytes",
    );
    assert_eq!(wrong.status(), 422);
    assert_eq!(f.counts(), (0, 0, 0));
    for (entry, bytes) in p.manifest.entries.iter().zip(&p.contents) {
        let response = server.send(
            "PUT",
            &format!("{request}/content/{}", entry.blake3),
            Some(&f.token),
            bytes,
        );
        assert_eq!(
            response.status(),
            200,
            "{}",
            response.into_string().unwrap()
        );
    }
    let (status, state) = server.command("POST", &format!("{request}/prepare"), &f.token);
    assert_eq!(status, 200, "{state}");
    assert_eq!(state["result"]["state"], "ready");
    let (status, committed) = server.command("POST", &format!("{request}/publish"), &f.token);
    assert_eq!(status, 200, "{committed}");
    assert_eq!(committed["result"]["state"], "committed");
    assert_eq!(f.counts(), (1, 1, 1));
    let (_, replayed) = server.command("POST", &format!("{request}/publish"), &f.token);
    assert_eq!(committed, replayed);
    let response = server.send("GET", &content, Some(&f.token), b"");
    assert_eq!(response.status(), 200);
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut response.into_reader(), &mut bytes).unwrap();
    assert_eq!(bytes, p.contents[1]);
    assert_eq!(
        server
            .command("GET", "/v1/repositories/repository", &f.token)
            .1["refs"]["refs/heads/main"],
        p.commit.to_string()
    );
    drop(server);
    let server = Running::start(&f);
    assert_eq!(server.command("GET", &request, &f.token).1, committed);
    let (_, old) = server.command(
        "GET",
        "/v1/repositories/repository/epochs/2/requests/owner/missing",
        &f.token,
    );
    assert_eq!(old["observation"], "unknown");
    f.store.revoke(&f.credential, &f.token).unwrap();
    assert_eq!(
        server.send("GET", &content, Some(&f.token), b"").status(),
        403
    );
    assert_eq!(f.counts(), (1, 1, 1));
}

#[test]
#[ignore = "requires explicitly isolated PostgreSQL and loopback HTTP; exercised by server acceptance and closing gate"]
fn http_rejects_cross_repository_and_actor_uploads_and_extra_bytes() {
    let mut f = Fixture::new();
    f.store
        .create_repository(&f.credential, "other", 100_000_000)
        .unwrap();
    let server = Running::start(&f);
    let p = Payload::new("scope-test", None);
    let begin = alt_server::BeginRequest {
        publication: p.publication.clone(),
        manifest: p.manifest.clone(),
    };
    let base = "/v1/repositories/repository/epochs/1/requests";
    assert_eq!(server.json("POST", base, &f.token, &begin).0, 200);
    for path in [
        format!(
            "/v1/repositories/other/epochs/1/requests/owner/scope-test/content/{}",
            p.manifest.entries[1].blake3
        ),
        format!(
            "{base}/intruder/scope-test/content/{}",
            p.manifest.entries[1].blake3
        ),
    ] {
        assert_eq!(
            server
                .send("PUT", &path, Some(&f.token), &p.contents[1])
                .status(),
            403
        );
    }
    let response = server.send(
        "PUT",
        &format!(
            "{base}/owner/scope-test/content/{}",
            p.manifest.entries[0].blake3
        ),
        Some(&f.token),
        b"one extra byte",
    );
    assert_eq!(response.status(), 413);
    assert_eq!(
        server
            .send("GET", "/v1/repositories/other", Some(&f.token), b"")
            .status(),
        403
    );
    assert_eq!(f.counts(), (0, 0, 0));
}

static NEXT: AtomicU64 = AtomicU64::new(1);
fn limits() -> Limits {
    Limits {
        request_bytes: 1_000_000,
        ref_updates: 100,
        record_roots: 100,
        manifest_entries: 1000,
        graph_objects: 1000,
        graph_edges: 10000,
        structured_bytes: 1_000_000,
        tree_paths: 1000,
        path_bytes: 4096,
        walk_steps: 100_000,
        chunk_bytes: 1_000_000,
        object_bytes: 10_000_000,
        total_bytes: 100_000_000,
    }
}
fn future() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
        + 3_600_000
}
fn grants() -> Vec<Grant> {
    vec![Grant {
        refs: RefSelector::Namespace("refs/".into()),
        create: true,
        update: true,
        delete: true,
        force: true,
        paths: None,
    }]
}
struct Fixture {
    key_seed: u8,
    admin_dsn: String,
    database: String,
    root: alt_os::temp::TempDir,
    spec: InstanceSpec,
    store: Store,
    token: String,
    credential: Credential,
}
impl Fixture {
    fn new() -> Self {
        Self::new_with_identity("installation", 7)
    }
    fn new_with_identity(installation: &str, key_seed: u8) -> Self {
        assert_eq!(
            std::env::var("ALT_SERVER_TEST_ISOLATED").as_deref(),
            Ok("1"),
            "database tests require an explicitly isolated PostgreSQL service"
        );
        let admin_dsn = std::env::var("ALT_SERVER_TEST_ADMIN_DSN")
            .expect("isolated PostgreSQL admin DSN required");
        let database = format!(
            "alt_test_{}_{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        );
        let mut client = Client::connect(&admin_dsn, NoTls).unwrap();
        client
            .batch_execute(&format!("CREATE DATABASE {database}"))
            .unwrap();
        let root = alt_os::temp::tempdir().unwrap();
        let spec = InstanceSpec {
            installation_id: installation.into(),
            workspace_id: "workspace".into(),
            deployment_mode: DeploymentMode::SelfHosted,
            runtime_generation: 1,
        };
        let mut config: postgres::Config = admin_dsn.parse().unwrap();
        config.dbname(&database);
        let mut store = Store::initialize(
            config.connect(NoTls).unwrap(),
            ByteStore::open(&root.path().join("bytes")).unwrap(),
            spec.clone(),
            SigningKey::from_bytes(&[key_seed; 32]),
            limits(),
        )
        .unwrap();
        let token = store.bootstrap_admin("owner", future()).unwrap();
        let credential = store.authenticate(&token).unwrap();
        store
            .create_repository(&credential, "repository", 100_000_000)
            .unwrap();
        store
            .set_grants(&credential, "repository", "owner", true, true, &grants())
            .unwrap();
        Self {
            key_seed,
            admin_dsn,
            database,
            root,
            spec,
            store,
            token,
            credential,
        }
    }
    fn client(&self) -> Client {
        let mut config: postgres::Config = self.admin_dsn.parse().unwrap();
        config.dbname(&self.database);
        config.connect(NoTls).unwrap()
    }
    fn counts(&self) -> (i64, i64, i64) {
        let row=self.client().query_one("SELECT (SELECT count(*) FROM alt_refs),(SELECT count(*) FROM alt_audit),(SELECT count(*) FROM alt_outbox)",&[]).unwrap();
        (row.get(0), row.get(1), row.get(2))
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let mut client = Client::connect(&self.admin_dsn, NoTls).unwrap();
        client
            .batch_execute(&format!("DROP DATABASE {} WITH (FORCE)", self.database))
            .unwrap();
    }
}
struct Payload {
    publication: Publication,
    manifest: Manifest,
    contents: Vec<Vec<u8>>,
    commit: ObjectId,
}
impl Payload {
    fn new(request: &str, parent: Option<ObjectId>) -> Self {
        let tree = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Tree, b"");
        let parents = parent.map(|p| format!("parent {p}\n")).unwrap_or_default();
        let commit_bytes=format!("tree {tree}\n{parents}author A <a@example.test> 1 +0000\ncommitter A <a@example.test> 1 +0000\n\n{request}\n").into_bytes();
        let commit = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Commit, &commit_bytes);
        let contents = vec![vec![], commit_bytes];
        let entries = contents
            .iter()
            .zip([(ContentKind::Tree, tree), (ContentKind::Commit, commit)])
            .map(|(bytes, (kind, oid))| {
                let digest = alt_hash::blake3::hash(bytes).to_hex().to_string();
                ContentEntry {
                    kind,
                    blake3: digest.clone(),
                    bytes: bytes.len() as u64,
                    git_oid: Some(oid.to_string()),
                    chunks: if bytes.is_empty() {
                        vec![]
                    } else {
                        vec![Chunk {
                            offset: 0,
                            bytes: bytes.len() as u64,
                            blake3: digest,
                        }]
                    },
                }
            })
            .collect();
        let manifest = Manifest {
            protocol_version: VERSION,
            object_format: ObjectFormat::Sha1,
            entries,
        };
        let publication = Publication {
            protocol_version: VERSION,
            key: RequestKey {
                scope: Scope {
                    installation_id: "installation".into(),
                    repository_instance_id: "repository".into(),
                    recovery_epoch: 1,
                },
                actor_id: "owner".into(),
                request_id: request.into(),
            },
            attribution: Attribution {
                kind: ActorKind::Human,
                on_behalf_of: None,
                delegation_id: None,
                agent_session: None,
            },
            object_format: ObjectFormat::Sha1,
            refs: vec![RefUpdate {
                name: "refs/heads/main".into(),
                expected: parent.map(|p| p.to_string()),
                new: Some(commit.to_string()),
                force: false,
            }],
            record_roots: vec![],
            content_manifest: manifest.id(limits()).unwrap(),
        };
        Self {
            publication,
            manifest,
            contents,
            commit,
        }
    }
}

#[test]
#[ignore = "requires isolated PostgreSQL and loopback HTTP; checks public liveness independently of fenced readiness"]
fn liveness_does_not_claim_readiness_after_runtime_fencing() {
    let f = Fixture::new();
    let server = Running::start(&f);
    let live = || {
        ureq::get(&format!("{}/livez", server.url))
            .call()
            .unwrap()
            .into_string()
            .unwrap()
    };
    assert_eq!(live(), "{\"alive\":true}");
    assert_eq!(
        ureq::get(&format!("{}/healthz", server.url))
            .call()
            .unwrap()
            .into_string()
            .unwrap(),
        "{\"ready\":true}"
    );
    f.client()
        .execute(
            "UPDATE alt_instance SET runtime_generation=runtime_generation+1",
            &[],
        )
        .unwrap();
    assert!(matches!(
        ureq::get(&format!("{}/healthz", server.url)).call(),
        Err(ureq::Error::Status(503, _))
    ));
    assert_eq!(live(), "{\"alive\":true}");
}
