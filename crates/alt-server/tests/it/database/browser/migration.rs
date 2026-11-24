//! A repository brought in through the browser: the cookie, the origin and the
//! token against forgery are asked of every upload.
use super::*;

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
    let ExportState::Available { envelope, .. } = crate::database::json::body(response) else {
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
            &alt_json::to_vec(&inspect).unwrap()
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
        &alt_json::to_vec(&inspect).unwrap(),
    );
    assert_eq!(checked.status(), 200);
    let checked: InspectedArchive = crate::database::json::body(checked);
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
            &alt_json::to_vec(&begin).unwrap()
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
            let current: ImportState = alt_json::from_value(current).unwrap();
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
        &alt_json::to_vec(&intent).unwrap(),
    );
    assert_eq!(response.status(), 200);
    let state: ImportState = crate::database::json::body(response);
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
            &alt_json::to_vec(&intent).unwrap()
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
