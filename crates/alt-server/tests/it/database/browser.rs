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
            let digest = blake3::hash(bytes).to_hex().to_string();
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
