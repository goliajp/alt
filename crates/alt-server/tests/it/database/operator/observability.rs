use super::*;
use std::{
    io::{BufRead, Read, Write},
    os::fd::AsRawFd,
    time::Instant,
};

struct ObservedServer {
    process: ServerProcess,
    url: String,
    log: PathBuf,
}
impl ObservedServer {
    fn start(db: &OperatorDb) -> Self {
        let log = db.root.path().join("http-observations.jsonl");
        let stdout = db.root.path().join("server-output.jsonl");
        let process = ServerProcess(
            db.command()
                .arg("serve")
                .env("RUST_LOG", "trace")
                .stdout(std::fs::File::create(&stdout).unwrap())
                .stderr(std::fs::File::create(&log).unwrap())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(line) = std::fs::read_to_string(&stdout).unwrap().lines().next()
                && let Ok(value) = serde_json::from_str::<serde_json::Value>(line)
                && let Some(address) = value["address"].as_str()
            {
                return Self {
                    process,
                    url: format!("http://{address}"),
                    log,
                };
            }
            assert!(Instant::now() < deadline, "operator did not start");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn call(&self, method: &str, path: &str, token: Option<&str>, bytes: &[u8]) -> ureq::Response {
        let mut request =
            ureq::request(method, &format!("{}{path}", self.url)).timeout(Duration::from_secs(5));
        if method == "POST" {
            request = request.set("Content-Type", "application/json");
        }
        if let Some(token) = token {
            request = request.set("Authorization", &format!("Bearer {token}"));
        }
        match request.send_bytes(bytes) {
            Ok(response) | Err(ureq::Error::Status(_, response)) => response,
            Err(error) => panic!("HTTP failed: {error}"),
        }
    }
    fn metrics(&self, token: &str) -> String {
        let response = self.call("GET", "/v1/instance/metrics", Some(token), b"");
        assert_eq!(response.status(), 200);
        assert_eq!(
            response.header("Content-Type"),
            Some("text/plain; version=0.0.4; charset=utf-8")
        );
        response.into_string().unwrap()
    }
    fn stop(&mut self) {
        // this unreaped child is owned by this fixture; its PID cannot have
        // been reused while the Child remains unreaped
        alt_os::sys::kill(
            self.process.0.id() as alt_os::sys::pid_t,
            alt_os::sys::SIGTERM,
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.process.0.try_wait().unwrap() {
                assert!(status.success());
                return;
            }
            assert!(
                Instant::now() < deadline,
                "server did not stop and drain observations"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn publish_large_stream(
    store: &mut Store,
    credential: &alt_server_store::Credential,
    parent: ObjectId,
) -> String {
    let bytes = vec![0x5a; 16 * 1024 * 1024];
    let blob = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, &bytes);
    let mut tree_bytes = b"100644 large.bin\0".to_vec();
    tree_bytes.extend_from_slice(blob.as_bytes());
    let tree = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Tree, &tree_bytes);
    let commit_bytes = format!("tree {tree}\nparent {parent}\nauthor A <a@example.test> 1 +0000\ncommitter A <a@example.test> 1 +0000\n\nLarge stream fixture\n").into_bytes();
    let commit = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Commit, &commit_bytes);
    let objects = [
        (ContentKind::Blob, blob, bytes),
        (ContentKind::Tree, tree, tree_bytes),
        (ContentKind::Commit, commit, commit_bytes),
    ];
    let limits = store.limits();
    let manifest = Manifest {
        protocol_version: VERSION,
        object_format: ObjectFormat::Sha1,
        entries: objects
            .iter()
            .map(|(kind, oid, bytes)| ContentEntry {
                kind: *kind,
                git_oid: Some(oid.to_string()),
                blake3: alt_hash::blake3::hash(bytes).to_hex().to_string(),
                bytes: bytes.len() as u64,
                chunks: bytes
                    .chunks(limits.chunk_bytes as usize)
                    .enumerate()
                    .map(|(i, chunk)| Chunk {
                        offset: i as u64 * limits.chunk_bytes,
                        bytes: chunk.len() as u64,
                        blake3: alt_hash::blake3::hash(chunk).to_hex().to_string(),
                    })
                    .collect(),
            })
            .collect(),
    };
    let mut publication = Payload::new("large-stream", Some(parent)).publication;
    publication.key.scope.installation_id = store.spec().installation_id.clone();
    publication.refs[0].new = Some(commit.to_string());
    publication.content_manifest = manifest.id(limits).unwrap();
    store
        .begin(credential, &publication, &manifest, 60_000)
        .unwrap();
    for (entry, (_, _, bytes)) in manifest.entries.iter().zip(&objects) {
        let proof = store
            .bytes()
            .put(
                &publication.key.scope,
                &entry.blake3,
                entry.bytes,
                limits.object_bytes,
                &mut &bytes[..],
            )
            .unwrap();
        store
            .register_content(credential, &publication.key, entry, &proof)
            .unwrap();
    }
    let work = store
        .acquire_publication(credential, &publication.key, 60_000, limits.graph_objects)
        .unwrap();
    let verified = alt_server::ValidatedWork::verify(work, limits, task_limits()).unwrap();
    verified.mark_ready(store, credential).unwrap();
    verified.publish(store, credential).unwrap();
    manifest.entries[0].blake3.clone()
}

fn disconnect_stream(server: &ObservedServer, token: &str, digest: &str) -> String {
    let address = server.url.strip_prefix("http://").unwrap();
    let mut socket = std::net::TcpStream::connect(address).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let size: alt_os::sys::c_int = 4096;
    // SAFETY: the owned live socket and pointer to an initialized integer are
    // valid for the complete setsockopt call. A small window keeps the 16MiB
    // response pending while the real client closes after receiving headers.
    assert_eq!(
        unsafe {
            alt_os::sys::setsockopt(
                socket.as_raw_fd(),
                alt_os::sys::SOL_SOCKET,
                alt_os::sys::SO_RCVBUF,
                &size as *const _ as *const alt_os::sys::c_void,
                std::mem::size_of_val(&size) as alt_os::sys::socklen_t,
            )
        },
        0
    );
    write!(socket,"GET /v1/repositories/repository/epochs/1/content/{digest} HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {token}\r\nConnection: close\r\n\r\n").unwrap();
    let mut socket = std::io::BufReader::new(socket);
    let mut status = String::new();
    socket.read_line(&mut status).unwrap();
    assert!(status.starts_with("HTTP/1.1 200"), "{status}");
    let mut trace = None;
    loop {
        let mut line = String::new();
        assert!(socket.read_line(&mut line).unwrap() > 0);
        if line == "\r\n" {
            break;
        }
        if let Some((key, value)) = line.split_once(':')
            && key.eq_ignore_ascii_case("x-alt-trace-id")
        {
            trace = Some(value.trim().to_owned());
        }
    }
    socket.read_exact(&mut [0u8; 1024]).unwrap();
    socket.get_ref().shutdown(std::net::Shutdown::Both).unwrap();
    trace.unwrap()
}

#[test]
#[ignore = "requires isolated PostgreSQL and a real server process; verifies actual log redaction, permissions and late transaction completion"]
fn request_observations_preserve_late_commits_and_current_metrics_authority() {
    let db = OperatorDb::new();
    let seed = db.path("receipt.key");
    let admin = db.path("admin.token");
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
    let mut config = Config::read(&db.config).unwrap();
    config.http.work_timeout_ms = 1000;
    config.limits.object_bytes = 20_000_000;
    std::fs::write(&db.config, serde_json::to_vec(&config).unwrap()).unwrap();
    let mut store = Store::open(
        Client::connect(&db.dsn, NoTls).unwrap(),
        ByteStore::open(&config.bytes_directory).unwrap(),
        config.instance.clone(),
        config.signing_key().unwrap(),
        config.limits,
    )
    .unwrap();
    let token = alt_server::operator::read_token(std::path::Path::new(&admin)).unwrap();
    let credential = store.authenticate(&token).unwrap();
    store
        .create_repository(&credential, "repository", 100_000_000)
        .unwrap();
    store
        .set_grants(&credential, "repository", "owner", true, true, &grants())
        .unwrap();
    let reader = store
        .issue_local_token(
            &credential,
            "reader",
            &Attribution {
                kind: ActorKind::Human,
                on_behalf_of: None,
                delegation_id: None,
                agent_session: None,
            },
            future(),
            false,
        )
        .unwrap();
    let mut server = ObservedServer::start(&db);
    let no_auth = server.call("GET", "/v1/instance/metrics", None, b"");
    assert_eq!(no_auth.status(), 401);
    let denied_trace = no_auth.header("x-alt-trace-id").unwrap().to_owned();
    assert_eq!(
        server
            .call("GET", "/v1/instance/metrics", Some(&reader), b"")
            .status(),
        403
    );
    assert!(
        server
            .metrics(&token)
            .contains("alt_backup_creation_known 0")
    );
    store
        .record_backup_event(&"b".repeat(64), alt_server_store::BackupEvent::Started)
        .unwrap();
    assert!(
        server
            .metrics(&token)
            .contains("alt_backup_creation_known 0")
    );
    store
        .record_backup_event(&"b".repeat(64), alt_server_store::BackupEvent::Created)
        .unwrap();
    let text = server.metrics(&token);
    assert!(text.contains("alt_backup_creation_known 1"));
    assert!(text.contains("alt_backup_age_known 1"));
    assert!(text.contains("alt_volume_available_bytes "));
    assert!(text.contains("alt_change_link_events_pending 0"));

    let invalid = ureq::get(&format!(
        "{}/unknown-secret-path?password=secret-query-sentinel",
        server.url
    ))
    .set("Cookie", "secret-cookie-sentinel")
    .set("X-Alt-Trace-Id", "secret-provided-trace")
    .set("Authorization", &format!("Bearer {token}"))
    .call();
    let invalid = match invalid {
        Err(ureq::Error::Status(404, response)) => response,
        _ => panic!("unknown endpoint accepted"),
    };
    assert_ne!(
        invalid.header("x-alt-trace-id"),
        Some("secret-provided-trace")
    );
    let base = "/v1/repositories/repository/epochs/1/requests";
    assert_eq!(
        server
            .call("POST", base, Some(&token), b"secret-body-sentinel")
            .status(),
        400
    );

    let mut payload = Payload::new("observed-request", None);
    payload.publication.key.scope.installation_id = config.instance.installation_id.clone();
    let begin = serde_json::to_vec(&alt_server::BeginRequest {
        publication: payload.publication.clone(),
        manifest: payload.manifest.clone(),
    })
    .unwrap();
    assert_eq!(
        server.call("POST", base, Some(&token), &begin).status(),
        200
    );
    let selected = format!("{base}/owner/observed-request");
    for (entry, bytes) in payload.manifest.entries.iter().zip(&payload.contents) {
        assert_eq!(
            server
                .call(
                    "PUT",
                    &format!("{selected}/content/{}", entry.blake3),
                    Some(&token),
                    bytes
                )
                .status(),
            200
        );
    }
    assert_eq!(
        server
            .call("POST", &format!("{selected}/prepare"), Some(&token), b"")
            .status(),
        200
    );
    // Repository row lock is after the HTTP credential preflight. The HTTP
    // deadline expires while the real publication worker remains alive.
    let mut blocker = Client::connect(&db.dsn, NoTls).unwrap();
    let mut transaction = blocker.transaction().unwrap();
    transaction
        .query_one(
            "SELECT id FROM alt_repositories WHERE id='repository' FOR UPDATE",
            &[],
        )
        .unwrap();
    let timed_out = server.call("POST", &format!("{selected}/publish"), Some(&token), b"");
    assert_eq!(timed_out.status(), 503);
    let late_trace = timed_out.header("x-alt-trace-id").unwrap().to_owned();
    let body: serde_json::Value = serde_json::from_reader(timed_out.into_reader()).unwrap();
    assert_eq!(body["code"], "outcome_unknown");
    transaction.commit().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let response = server.call("GET", &selected, Some(&token), b"");
        if response.status() == 200 {
            let value: serde_json::Value = serde_json::from_reader(response.into_reader()).unwrap();
            if value["result"]["state"] == "committed" {
                break;
            }
        }
        assert!(
            Instant::now() < deadline,
            "detached publication did not complete"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let text = server.metrics(&token);
    assert!(text.contains("alt_operation_outcomes_total{outcome=\"ok\",phase=\"commit\"} 1"));
    assert!(text.contains("alt_work_bytes_total{phase=\"upload\"}"));
    assert!(
        !text.contains("observed-request") && !text.contains("secret-") && !text.contains(&token)
    );
    let replay = server.call("POST", &format!("{selected}/publish"), Some(&token), b"");
    assert_eq!(replay.status(), 200);
    let replay_trace = replay.header("x-alt-trace-id").unwrap().to_owned();
    let value: serde_json::Value = serde_json::from_reader(replay.into_reader()).unwrap();
    assert_eq!(value["result"]["state"], "committed");
    assert_ne!(replay_trace, late_trace);
    assert!(
        server
            .metrics(&token)
            .contains("alt_operation_outcomes_total{outcome=\"ok\",phase=\"commit\"} 1")
    );
    let digest = publish_large_stream(&mut store, &credential, payload.commit);
    let completed = server.call(
        "GET",
        &format!("/v1/repositories/repository/epochs/1/content/{digest}"),
        Some(&token),
        b"",
    );
    assert_eq!(completed.status(), 200);
    let completed_trace = completed.header("x-alt-trace-id").unwrap().to_owned();
    let mut downloaded = Vec::new();
    completed
        .into_reader()
        .read_to_end(&mut downloaded)
        .unwrap();
    assert_eq!(downloaded, vec![0x5a; 16 * 1024 * 1024]);
    let stream_trace = disconnect_stream(&server, &token, &digest);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let logged = std::fs::read_to_string(&server.log).unwrap();
        if logged
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .any(|v| {
                v["fields"]["trace_id"] == stream_trace && v["fields"]["event"] == "response.ended"
            })
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "stream cancellation was not observed"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    store.revoke(&credential, &token).unwrap();
    assert_eq!(
        server
            .call("GET", "/v1/instance/metrics", Some(&token), b"")
            .status(),
        403
    );
    server.stop();
    let text = std::fs::read_to_string(&server.log).unwrap();
    for secret in [
        &token,
        &reader,
        "secret-query-sentinel",
        "unknown-secret-path",
        "secret-cookie-sentinel",
        "secret-provided-trace",
        "secret-body-sentinel",
        &db.dsn,
    ] {
        assert!(
            !text.contains(secret),
            "request or backend secret entered operational logs"
        );
    }
    let events: Vec<serde_json::Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(
        events
            .iter()
            .any(|v| v["fields"]["trace_id"] == denied_trace && v["fields"]["http_status"] == 401)
    );
    let response_index = events
        .iter()
        .position(|v| {
            v["fields"]["trace_id"] == late_trace
                && v["fields"]["event"] == "response.headers"
                && v["fields"]["http_status"] == 503
        })
        .unwrap();
    let commit_index = events
        .iter()
        .position(|v| {
            v["fields"]["trace_id"] == late_trace
                && v["fields"]["event"] == "work.ended"
                && v["fields"]["phase"] == "commit"
                && v["fields"]["outcome"] == "ok"
        })
        .unwrap();
    assert!(commit_index > response_index);
    let committed = &events[commit_index]["fields"];
    assert_eq!(committed["request_id"], "observed-request");
    assert_eq!(committed["transaction_id"], "1");
    assert_eq!(committed["actor_kind"], "human");
    let replay = events
        .iter()
        .find(|v| {
            v["fields"]["trace_id"] == replay_trace && v["fields"]["event"] == "response.headers"
        })
        .unwrap();
    assert_eq!(replay["fields"]["request_id"], "observed-request");
    assert_eq!(replay["fields"]["transaction_id"], "1");
    assert!(
        !events
            .iter()
            .any(|v| v["fields"]["trace_id"] == replay_trace && v["fields"]["phase"] == "commit")
    );
    let completed: Vec<_> = events
        .iter()
        .filter(|v| {
            v["fields"]["trace_id"] == completed_trace && v["fields"]["event"] == "response.ended"
        })
        .collect();
    assert_eq!(completed.len(), 1);
    assert_eq!(completed[0]["fields"]["outcome"], "complete");
    assert_eq!(
        completed[0]["fields"]["body_bytes_yielded"],
        16 * 1024 * 1024
    );
    let terminal: Vec<_> = events
        .iter()
        .filter(|v| {
            v["fields"]["trace_id"] == stream_trace && v["fields"]["event"] == "response.ended"
        })
        .collect();
    assert_eq!(terminal.len(), 1);
    assert_eq!(terminal[0]["fields"]["outcome"], "dropped");
    assert!(
        terminal[0]["fields"]["body_bytes_yielded"]
            .as_u64()
            .unwrap()
            < 16 * 1024 * 1024
    );
}
