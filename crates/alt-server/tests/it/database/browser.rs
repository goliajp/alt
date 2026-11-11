use super::*;
use alt_browser_auth::PublicOrigin;
use serde_json::{Value, json};
const ORIGIN: &str = "https://workbench.example.test";
const PASSWORD: &str = "a private browser acceptance password";
fn start(f: &mut Fixture) -> Running {
    let origin = PublicOrigin::parse(ORIGIN).unwrap();
    f.store.enable_browser(&f.credential, &origin).unwrap();
    let assets = f.root.path().join("assets");
    std::fs::create_dir_all(&assets).unwrap();
    std::fs::write(
        assets.join("index.html"),
        "<!doctype html><title>alt</title>",
    )
    .unwrap();
    Running::start_with_browser(
        f,
        Some(alt_server::WebOptions {
            origin,
            assets: alt_server::WebAssets::load(&assets).unwrap(),
            session_ttl_ms: 60_000,
        }),
    )
}
fn user(f: &mut Fixture, actor: &str, administrator: bool) {
    f.store
        .create_local_user(&f.credential, actor, actor, actor, PASSWORD, administrator)
        .unwrap();
}
fn send(
    server: &Running,
    method: &str,
    path: &str,
    cookie: Option<&str>,
    origin: Option<&str>,
    csrf: Option<&str>,
    body: Option<Value>,
) -> ureq::Response {
    let mut request =
        ureq::request(method, &format!("{}{path}", server.url)).timeout(Duration::from_secs(15));
    if let Some(cookie) = cookie {
        request = request.set("Cookie", cookie);
    }
    if let Some(origin) = origin {
        request = request.set("Origin", origin);
    }
    if let Some(csrf) = csrf {
        request = request.set("X-Alt-CSRF", csrf);
    }
    let response = if let Some(body) = body {
        request
            .set("Content-Type", "application/json")
            .send_bytes(&serde_json::to_vec(&body).unwrap())
    } else {
        request.call()
    };
    match response {
        Ok(r) | Err(ureq::Error::Status(_, r)) => r,
        Err(error) => panic!("browser transport: {error}"),
    }
}
fn login(server: &Running, actor: &str) -> (String, Value) {
    let response = send(
        server,
        "POST",
        "/web/login",
        None,
        Some(ORIGIN),
        None,
        Some(json!({"login_name":actor,"password":PASSWORD})),
    );
    assert_eq!(
        response.status(),
        200,
        "{}",
        response.into_string().unwrap()
    );
    let cookie = response.header("Set-Cookie").unwrap().to_owned();
    assert!(cookie.starts_with("__Host-alt_session="));
    assert!(
        cookie.contains("; Secure")
            && cookie.contains("; HttpOnly")
            && cookie.contains("; SameSite=Strict")
    );
    assert!(!cookie.contains("Domain="));
    let cookie = cookie.split(';').next().unwrap().to_owned();
    let session: Value = serde_json::from_reader(response.into_reader()).unwrap();
    assert_eq!(session["user"]["actor_id"], actor);
    assert!(!serde_json::to_string(&session).unwrap().contains("alt1_"));
    (cookie, session)
}
fn get(server: &Running, path: &str, cookie: &str) -> (u16, Value) {
    let response = send(server, "GET", path, Some(cookie), None, None, None);
    (
        response.status(),
        serde_json::from_reader(response.into_reader()).unwrap(),
    )
}
#[test]
#[ignore = "requires isolated PostgreSQL and loopback HTTP; exercised by browser acceptance and closing gate"]
fn cookie_sessions_csrf_revocation_and_origin_binding_are_enforced_over_http() {
    let mut f = Fixture::new();
    user(&mut f, "browser", true);
    user(&mut f, "member", false);
    let server = start(&mut f);
    assert_eq!(
        send(&server, "GET", "/web/session", None, None, None, None).status(),
        401
    );
    assert_eq!(
        send(
            &server,
            "POST",
            "/web/login",
            None,
            Some("https://other.example.test"),
            None,
            Some(json!({"login_name":"browser","password":PASSWORD}))
        )
        .status(),
        403
    );
    assert_eq!(
        f.client()
            .query_one("SELECT count(*) FROM alt_web_sessions", &[])
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    let (cookie, session) = login(&server, "browser");
    let proof = session["csrf"].as_str().unwrap();
    assert_eq!(
        send(&server, "GET", "/web/instance", None, None, None, None).status(),
        401
    );
    let (member_cookie, _) = login(&server, "member");
    assert_eq!(get(&server, "/web/instance", &member_cookie).0, 403);
    assert_eq!(get(&server, "/web/instance/audit", &member_cookie).0, 403);
    let (status, operations) = get(&server, "/web/instance", &cookie);
    assert_eq!(status, 200);
    assert_eq!(operations["repositories"], "1");
    assert_eq!(operations["runtime_generation"], "1");
    assert!(operations["last_restore"].is_null());
    assert_eq!(get(&server, "/web/instance/audit", &cookie).0, 200);
    assert_ne!(get(&server, "/web/instance/audit?after=01", &cookie).0, 200);

    assert_eq!(get(&server, "/web/session", &cookie).1, session);
    assert_eq!(
        get(&server, "/web/repositories", &cookie).1["items"],
        json!([])
    );
    f.store
        .set_grants(&f.credential, "repository", "browser", true, false, &[])
        .unwrap();
    assert_eq!(
        get(&server, "/web/repositories", &cookie).1["items"][0]["scope"]["repository_instance_id"],
        "repository"
    );
    assert_eq!(get(&server, "/web/repositories/absent", &cookie).0, 403);
    let (other_cookie, _) = login(&server, "browser");
    let (status, sessions) = get(&server, "/web/sessions", &cookie);
    assert_eq!(status, 200, "{sessions}");
    let other_id = sessions
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["current"] == false)
        .unwrap()["id"]
        .as_str()
        .unwrap();
    assert_eq!(
        send(
            &server,
            "POST",
            &format!("/web/sessions/{}/revoke", "0".repeat(64)),
            Some(&cookie),
            Some(ORIGIN),
            Some(proof),
            Some(json!({}))
        )
        .status(),
        403
    );
    assert_eq!(
        send(
            &server,
            "POST",
            &format!("/web/sessions/{other_id}/revoke"),
            Some(&cookie),
            Some(ORIGIN),
            Some(proof),
            Some(json!({}))
        )
        .status(),
        200
    );
    assert_eq!(get(&server, "/web/session", &other_cookie).0, 401);
    let duplicate = format!("{cookie}; {cookie}");
    assert_eq!(get(&server, "/web/session", &duplicate).0, 401);
    for (origin, csrf) in [
        (None, Some(proof)),
        (Some(ORIGIN), None),
        (Some("null"), Some(proof)),
        (Some(ORIGIN), Some("wrong")),
    ] {
        assert_eq!(
            send(
                &server,
                "POST",
                "/web/logout",
                Some(&cookie),
                origin,
                csrf,
                Some(json!({}))
            )
            .status(),
            403
        );
    }
    assert_eq!(get(&server, "/web/session", &cookie).0, 200);
    let response = send(
        &server,
        "POST",
        "/web/logout",
        Some(&cookie),
        Some(ORIGIN),
        Some(proof),
        Some(json!({})),
    );
    assert_eq!(response.status(), 200);
    assert!(response.header("Set-Cookie").unwrap().contains("Max-Age=0"));
    assert_eq!(get(&server, "/web/session", &cookie).0, 401);
    assert_eq!(get(&server, "/web/repositories", &cookie).0, 403);
    let (cookie, _) = login(&server, "browser");
    f.store
        .disable_local_user(&f.credential, "browser", true)
        .unwrap();
    assert_eq!(get(&server, "/web/session", &cookie).0, 401);
    // Runtime refuses missing config after persistent browser enablement.
    assert!(f.store.check_browser_binding(None).is_err());
    // and says why, instead of leaving the operator with an unknown outcome
    assert_eq!(
        Running::refusal(&f, None),
        "the browser origin recorded in the database differs from the configured one"
    );
    let response = send(&server, "GET", "/", None, None, None, None);
    assert_eq!(response.status(), 200);
    assert!(
        response
            .header("Content-Security-Policy")
            .unwrap()
            .contains("frame-ancestors 'none'")
    );
    assert_eq!(
        send(&server, "GET", "/.env", None, None, None, None).status(),
        404
    );
    assert_eq!(
        send(&server, "GET", "/healthz", None, None, None, None).status(),
        200
    );
}
#[test]
#[ignore = "requires isolated PostgreSQL and loopback HTTP; exercised by browser acceptance and closing gate"]
fn browser_browsing_observes_only_published_bytes_and_current_repository_grants() {
    let mut f = Fixture::new();
    user(&mut f, "reader", false);
    f.store
        .set_grants(&f.credential, "repository", "reader", true, false, &[])
        .unwrap();
    let server = start(&mut f);
    let (cookie, _) = login(&server, "reader");
    let mut p = Payload::new("browser-content", None);
    let blob = b"original browser bytes\n".to_vec();
    let blob_oid = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, &blob);
    let mut tree = b"100644 file\0".to_vec();
    tree.extend_from_slice(blob_oid.as_bytes());
    let tree_oid = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Tree, &tree);
    let commit = format!("tree {tree_oid}\nauthor A <a@example.test> 1 +0000\ncommitter A <a@example.test> 1 +0000\n\noriginal browser content\n").into_bytes();
    p.commit = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Commit, &commit);
    p.contents = vec![blob, tree, commit];
    p.manifest.entries = p
        .contents
        .iter()
        .zip([
            (ContentKind::Blob, blob_oid),
            (ContentKind::Tree, tree_oid),
            (ContentKind::Commit, p.commit),
        ])
        .map(|(bytes, (kind, oid))| {
            let digest = alt_hash::blake3::hash(bytes).to_hex().to_string();
            ContentEntry {
                kind,
                blake3: digest.clone(),
                bytes: bytes.len() as u64,
                git_oid: Some(oid.to_string()),
                chunks: vec![Chunk {
                    offset: 0,
                    bytes: bytes.len() as u64,
                    blake3: digest,
                }],
            }
        })
        .collect();
    p.publication.refs[0].new = Some(p.commit.to_string());
    p.publication.content_manifest = p.manifest.id(limits()).unwrap();
    let base = "/v1/repositories/repository/epochs/1/requests";
    let req = format!("{base}/owner/browser-content");
    assert_eq!(
        server
            .json(
                "POST",
                base,
                &f.token,
                &alt_server::BeginRequest {
                    publication: p.publication.clone(),
                    manifest: p.manifest.clone()
                }
            )
            .0,
        200
    );
    for (entry, bytes) in p.manifest.entries.iter().zip(&p.contents) {
        assert_eq!(
            server
                .send(
                    "PUT",
                    &format!("{req}/content/{}", entry.blake3),
                    Some(&f.token),
                    bytes
                )
                .status(),
            200
        );
    }
    let commit = p.commit.to_string();
    assert_eq!(
        get(
            &server,
            &format!("/web/repositories/repository/tree?commit={commit}"),
            &cookie
        )
        .0,
        403
    );
    assert_eq!(
        server
            .command("POST", &format!("{req}/publish"), &f.token)
            .0,
        200
    );
    assert_eq!(
        get(&server, "/web/repositories/repository", &cookie).1["refs"]["refs/heads/main"],
        commit
    );
    let (status, tree) = get(
        &server,
        &format!("/web/repositories/repository/tree?commit={commit}"),
        &cookie,
    );
    assert_eq!(status, 200, "{tree}");
    assert_eq!(tree["entries"][0]["name"], "file");
    let oid = tree["entries"][0]["oid"].as_str().unwrap();
    let blob_path = format!("/web/repositories/repository/objects/{oid}");
    let (status, blob) = get(&server, &blob_path, &cookie);
    assert_eq!(status, 200, "{blob}");
    let original = send(
        &server,
        "GET",
        &format!("{blob_path}/download"),
        Some(&cookie),
        None,
        None,
        None,
    );
    assert_eq!(original.status(), 200);
    assert!(
        original
            .header("Content-Disposition")
            .unwrap()
            .starts_with("attachment")
    );
    assert_eq!(
        original.into_string().unwrap(),
        blob["text"].as_str().unwrap()
    );
    assert_eq!(
        get(&server, "/web/repositories/repository/commits", &cookie).1["items"][0]["oid"],
        commit
    );
    f.store
        .set_grants(&f.credential, "repository", "reader", false, false, &[])
        .unwrap();
    assert_eq!(get(&server, &blob_path, &cookie).0, 403);
    assert_eq!(
        get(&server, "/web/repositories", &cookie).1["items"],
        json!([])
    );
    assert_eq!(get(&server, "/web/users", &cookie).0, 403);
}

fn migration_send(
    server: &Running,
    method: &str,
    path: &str,
    cookie: Option<&str>,
    origin: Option<&str>,
    csrf: Option<&str>,
    body: &[u8],
) -> ureq::Response {
    let mut request = ureq::request(method, &format!("{}{path}", server.url))
        .timeout(Duration::from_secs(15))
        .set(
            "Content-Type",
            if method == "PUT" {
                "application/octet-stream"
            } else {
                "application/json"
            },
        );
    if let Some(v) = cookie {
        request = request.set("Cookie", v);
    }
    if let Some(v) = origin {
        request = request.set("Origin", v);
    }
    if let Some(v) = csrf {
        request = request.set("X-Alt-CSRF", v);
    }
    match request.send_bytes(body) {
        Ok(v) | Err(ureq::Error::Status(_, v)) => v,
        Err(e) => panic!(
            "browser migration: method={method} matching_origin={} csrf_present={} bytes={}: {e}",
            origin == Some(ORIGIN),
            csrf.is_some(),
            body.len()
        ),
    }
}
/// Unauthorized uploads must be refused from their headers. Sending a whole
/// body before reading an early rejection races the TCP close and can turn a
/// real 403 into a client-side BrokenPipe. Expect/continue makes that boundary
/// observable without weakening either the status or the no-upload assertion.
fn migration_rejected_before_body(
    server: &Running,
    path: &str,
    cookie: Option<&str>,
    origin: &str,
    csrf: Option<&str>,
    bytes: usize,
) {
    use std::io::{Read, Write};
    let address = server.url.strip_prefix("http://").unwrap();
    let mut socket = std::net::TcpStream::connect(address).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(15)))
        .unwrap();
    socket
        .set_write_timeout(Some(Duration::from_secs(15)))
        .unwrap();
    let mut headers = format!(
        "PUT {path} HTTP/1.1\r\nHost: {address}\r\nContent-Type: application/octet-stream\r\nContent-Length: {bytes}\r\nExpect: 100-continue\r\nConnection: close\r\nOrigin: {origin}\r\n"
    );
    if let Some(cookie) = cookie {
        headers.push_str(&format!("Cookie: {cookie}\r\n"));
    }
    if let Some(csrf) = csrf {
        headers.push_str(&format!("X-Alt-CSRF: {csrf}\r\n"));
    }
    headers.push_str("\r\n");
    socket.write_all(headers.as_bytes()).unwrap();
    let mut response = String::new();
    socket.take(65_537).read_to_string(&mut response).unwrap();
    assert!(response.len() <= 65_536);
    assert!(
        response.starts_with("HTTP/1.1 403 "),
        "Expected a final rejection before 100 Continue: {response}"
    );
    assert!(response.contains("application/json"), "{response}");
}

fn migration_payload() -> Payload {
    let mut payload = Payload::new("browser-portable", None);
    let blob_bytes = vec![b'x'; 65536];
    let blob = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, &blob_bytes);
    let mut tree_bytes = b"100644 migration.bin\0".to_vec();
    tree_bytes.extend_from_slice(blob.as_bytes());
    let tree = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Tree, &tree_bytes);
    let commit_bytes = format!("tree {tree}\nauthor A <a@example.test> 1 +0000\ncommitter A <a@example.test> 1 +0000\n\nbrowser migration\n").into_bytes();
    let commit = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Commit, &commit_bytes);
    payload.contents = vec![blob_bytes, tree_bytes, commit_bytes];
    payload.manifest.entries = payload
        .contents
        .iter()
        .zip([
            (ContentKind::Blob, blob),
            (ContentKind::Tree, tree),
            (ContentKind::Commit, commit),
        ])
        .map(|(bytes, (kind, oid))| {
            let blake3 = alt_hash::blake3::hash(bytes).to_hex().to_string();
            ContentEntry {
                kind,
                git_oid: Some(oid.to_string()),
                blake3: blake3.clone(),
                bytes: bytes.len() as u64,
                chunks: vec![Chunk {
                    offset: 0,
                    bytes: bytes.len() as u64,
                    blake3,
                }],
            }
        })
        .collect();
    payload.commit = commit;
    payload.publication.refs[0].new = Some(commit.to_string());
    payload.publication.content_manifest = payload.manifest.id(limits()).unwrap();
    payload
}
#[test]
#[ignore = "requires isolated PostgreSQL and loopback HTTP; browser portable archive, large upload and current session checks"]
fn browser_migration_requires_current_cookie_origin_and_csrf_for_binary_uploads() {
    use alt_portable::http::{ExportState, ImportState, InspectArchive, InspectedArchive};
    let mut source = Fixture::new();
    let mut target = Fixture::new_with_identity("browser-destination", 8);
    for fixture in [&mut source, &mut target] {
        user(fixture, "browser", true);
        fixture
            .store
            .set_grants(
                &fixture.credential,
                "repository",
                "browser",
                true,
                true,
                &grants(),
            )
            .unwrap();
    }
    let a = start(&mut source);
    let b = start(&mut target);
    let native = super::portable::client(&a, &source.token);
    let payload = migration_payload();
    super::portable::publish(&native, &payload);
    let (source_cookie, source_session) = login(&a, "browser");
    let (target_cookie, target_session) = login(&b, "browser");
    let source_csrf = source_session["csrf"].as_str().unwrap();
    let target_csrf = target_session["csrf"].as_str().unwrap();
    let export_path = "/web/repositories/repository/epochs/1/portable/exports/browser-export";
    for (cookie, origin, csrf, expected) in [
        (None, Some(ORIGIN), Some(source_csrf), 403),
        (Some(source_cookie.as_str()), Some(ORIGIN), None, 403),
        (
            Some(source_cookie.as_str()),
            Some("https://foreign.example.test"),
            Some(source_csrf),
            403,
        ),
    ] {
        assert_eq!(
            migration_send(&a, "POST", export_path, cookie, origin, csrf, b"").status(),
            expected
        );
    }
    let response = migration_send(
        &a,
        "POST",
        export_path,
        Some(&source_cookie),
        Some(ORIGIN),
        Some(source_csrf),
        b"",
    );
    assert_eq!(response.status(), 200);
    let ExportState::Available { envelope, .. } =
        serde_json::from_reader(response.into_reader()).unwrap()
    else {
        panic!("export available")
    };
    let package = super::portable::bounds()
        .verification(limits(), task_limits())
        .unwrap()
        .package;
    let archive = send(
        &a,
        "GET",
        &format!("{export_path}/archive"),
        Some(&source_cookie),
        None,
        None,
        None,
    );
    assert_eq!(archive.status(), 200);
    assert_eq!(
        archive.header("Content-Disposition"),
        Some("attachment; filename=\"alt-snapshot.altportable\"")
    );
    let mut archive_bytes = Vec::new();
    archive
        .into_reader()
        .read_to_end(&mut archive_bytes)
        .unwrap();
    alt_portable::read_archive(
        &mut &archive_bytes[..],
        &envelope.catalog.source,
        &SigningKey::from_bytes(&[7; 32]).verifying_key().to_bytes(),
        package,
        |_| Ok(Box::new(std::io::sink())),
    )
    .unwrap();
    let inspect_path = "/web/repositories/repository/epochs/1/portable/inspect";
    let mut inspect = InspectArchive {
        source: envelope.catalog.source.clone(),
        source_public_key: source.store.receipt_public_key(),
        metadata: String::from_utf8(envelope.encode(package).unwrap()).unwrap(),
    };
    let original_key = inspect.source_public_key.clone();
    inspect.source_public_key = target.store.receipt_public_key();
    assert_eq!(
        migration_send(
            &b,
            "POST",
            inspect_path,
            Some(&target_cookie),
            Some(ORIGIN),
            Some(target_csrf),
            &serde_json::to_vec(&inspect).unwrap()
        )
        .status(),
        422
    );
    inspect.source_public_key = original_key;
    let checked = migration_send(
        &b,
        "POST",
        inspect_path,
        Some(&target_cookie),
        Some(ORIGIN),
        Some(target_csrf),
        &serde_json::to_vec(&inspect).unwrap(),
    );
    assert_eq!(checked.status(), 200);
    let checked: InspectedArchive = serde_json::from_reader(checked.into_reader()).unwrap();
    let intent = alt_portable::ImportIntent {
        version: 1,
        key: RequestKey {
            scope: target
                .store
                .repository_info(&target.credential, "repository")
                .unwrap()
                .scope,
            actor_id: "browser".into(),
            request_id: "browser-import".into(),
        },
        target_public_key: target.store.receipt_public_key(),
        source: envelope.catalog.source.clone(),
        source_public_key: source.store.receipt_public_key(),
        catalog_digest: checked.catalog_digest,
        mappings: envelope
            .catalog
            .identities
            .iter()
            .map(|i| alt_portable::IdentityMapping {
                installation_id: i.installation_id.clone(),
                actor_id: i.actor_id.clone(),
                target_actor: None,
            })
            .collect(),
    };
    let import_path =
        "/web/repositories/repository/epochs/1/portable/imports/browser/browser-import";
    let begin = alt_portable::http::BeginImport {
        intent: intent.clone(),
        envelope: (*envelope).clone(),
    };
    assert_eq!(
        migration_send(
            &b,
            "POST",
            import_path,
            Some(&target_cookie),
            Some(ORIGIN),
            Some(target_csrf),
            &serde_json::to_vec(&begin).unwrap()
        )
        .status(),
        200
    );
    for blob in &envelope.catalog.blobs {
        let mut bytes = Vec::new();
        native
            .portable_download(&envelope.catalog.source, "browser-export", blob, &mut bytes)
            .unwrap();
        let path = format!("{import_path}/content/{}", blob.blake3);
        for (cookie, origin, csrf) in [
            (None, ORIGIN, Some(target_csrf)),
            (Some(target_cookie.as_str()), ORIGIN, None),
            (
                Some(target_cookie.as_str()),
                "https://foreign.example.test",
                Some(target_csrf),
            ),
        ] {
            migration_rejected_before_body(&b, &path, cookie, origin, csrf, bytes.len());
            let (status, current) = get(&b, import_path, &target_cookie);
            assert_eq!(status, 200, "{current}");
            let current: ImportState = serde_json::from_value(current).unwrap();
            let ImportState::Receiving { missing, .. } = current else {
                panic!("rejection cannot activate the import")
            };
            assert!(
                missing.iter().any(|entry| entry.blake3 == blob.blake3),
                "rejected upload must still be missing"
            );
        }
        assert_eq!(
            migration_send(
                &b,
                "PUT",
                &path,
                Some(&target_cookie),
                Some(ORIGIN),
                Some(target_csrf),
                &bytes
            )
            .status(),
            200
        );
    }
    let activate = format!("{import_path}/activate");
    let response = migration_send(
        &b,
        "POST",
        &activate,
        Some(&target_cookie),
        Some(ORIGIN),
        Some(target_csrf),
        &serde_json::to_vec(&intent).unwrap(),
    );
    assert_eq!(response.status(), 200);
    let state: ImportState = serde_json::from_reader(response.into_reader()).unwrap();
    assert!(matches!(state, ImportState::Committed { .. }));
    assert_eq!(
        target
            .store
            .repository_info(&target.credential, "repository")
            .unwrap()
            .refs["refs/heads/main"],
        payload.commit.to_string()
    );
    // Revocation remains a current session fact, even with an intact original
    // CSRF proof and a known completed import identity.
    let logout = send(
        &b,
        "POST",
        "/web/logout",
        Some(&target_cookie),
        Some(ORIGIN),
        Some(target_csrf),
        Some(json!({})),
    );
    assert_eq!(logout.status(), 200);
    assert_eq!(get(&b, import_path, &target_cookie).0, 403);
    assert_eq!(
        migration_send(
            &b,
            "POST",
            &activate,
            Some(&target_cookie),
            Some(ORIGIN),
            Some(target_csrf),
            &serde_json::to_vec(&intent).unwrap()
        )
        .status(),
        403
    );
    assert_eq!(
        target
            .client()
            .query_one("SELECT count(*) FROM alt_requests", &[])
            .unwrap()
            .get::<_, i64>(0),
        0
    );
}

#[test]
#[ignore = "requires isolated PostgreSQL and actual browser HTTP authentication"]
fn change_links_use_current_browser_scope_and_csrf() {
    let mut f = Fixture::new();
    user(&mut f, "browser", true);
    user(&mut f, "ungranted", false);
    f.store
        .set_grants(
            &f.credential,
            "repository",
            "browser",
            true,
            true,
            &grants(),
        )
        .unwrap();
    let origin = PublicOrigin::parse(ORIGIN).unwrap();
    f.store.enable_browser(&f.credential, &origin).unwrap();
    let assets = f.root.path().join("link-assets");
    std::fs::create_dir(&assets).unwrap();
    std::fs::write(
        assets.join("index.html"),
        "<!doctype html><title>alt</title>",
    )
    .unwrap();
    let (config, runtime) = super::change_links::channel(&mut f, "http://127.0.0.1:1");
    let server = Running::start_with_links(
        &f,
        Some(alt_server::WebOptions {
            origin,
            assets: alt_server::WebAssets::load(&assets).unwrap(),
            session_ttl_ms: 60000,
        }),
        None,
        vec![runtime],
    );
    let (cookie, session) = login(&server, "browser");
    let (outsider, _) = login(&server, "ungranted");
    let path = "/web/repositories/repository/epochs/1/changes";
    let channels = format!("{path}/channels");
    assert_eq!(get(&server, &channels, &outsider).0, 403);
    let (status, values) = get(&server, &channels, &cookie);
    assert_eq!(status, 200, "{values}");
    assert_eq!(
        values[0]["peer_browser_origin"],
        "https://sentori.example.test"
    );
    assert_eq!(server.command("GET", &channels, &f.token).0, 401);
    assert_eq!(
        get(&server, &channels.replace("epochs/1", "epochs/2"), &cookie).0,
        403
    );
    let request = super::change_links::repair_request(&f, &config);
    assert_eq!(
        server
            .json(
                "POST",
                golia_change_link_http::INBOX_PATH,
                &f.token,
                &request
            )
            .0,
        200
    );
    let digest = request.digest().unwrap();
    let (status, history) = get(&server, &format!("{path}/events/{digest}"), &cookie);
    assert_eq!(status, 200);
    assert_eq!(
        history["statement"],
        serde_json::to_value(&request).unwrap()
    );
    let mapping = format!("{path}/mappings");
    assert_eq!(
        send(
            &server,
            "POST",
            &mapping,
            Some(&cookie),
            Some(ORIGIN),
            None,
            Some(json!({}))
        )
        .status(),
        403
    );
    let csrf = session["csrf"].as_str().unwrap();
    assert_eq!(
        send(
            &server,
            "POST",
            &mapping,
            Some(&cookie),
            Some("https://foreign.test"),
            Some(csrf),
            Some(json!({}))
        )
        .status(),
        403
    );
    // Valid browser authentication reaches the same strict business decoder.
    assert_eq!(
        send(
            &server,
            "POST",
            &mapping,
            Some(&cookie),
            Some(ORIGIN),
            Some(csrf),
            Some(json!({}))
        )
        .status(),
        400
    );
    f.store
        .disable_local_user(&f.credential, "browser", true)
        .unwrap();
    assert_eq!(
        get(&server, &format!("{path}/events/{digest}"), &cookie).0,
        403
    );
}
