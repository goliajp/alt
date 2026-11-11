use super::*;
use golia_platform_contract::{
    AccountAuthorization, AltLimits, AltServiceAuthorization, Audience, Capability, Counter, Lease,
    Product, Secret,
};
use std::sync::{Arc, atomic::AtomicUsize};
use tiny_http::{Header, Response, Server};
fn secret(value: &str) -> Secret {
    value.to_owned().try_into().unwrap()
}
fn time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
fn lease(audience: &Audience, sequence: u64) -> Lease {
    Lease {
        version: 1,
        audience: audience.clone(),
        sequence: Counter(sequence),
        revocation_generation: Counter(1),
        issued_at_unix_ms: Counter(time()),
        expires_at_unix_ms: Counter(time() + 60_000),
    }
}

#[test]
#[ignore = "requires isolated PostgreSQL and actual loopback HTTP platform fixture"]
fn cloud_http_refresh_outage_revocation_and_browser_identity() {
    let mut f = Fixture::new();
    let provider = Server::http("127.0.0.1:0").unwrap();
    let audience = Audience {
        organization_id: "organization".to_string().try_into().unwrap(),
        workspace_id: f.spec.workspace_id.clone().try_into().unwrap(),
        installation_id: f.spec.installation_id.clone().try_into().unwrap(),
        product: Product::Alt,
        generation: Counter(1),
        origin: "https://alt.example.test".to_string().try_into().unwrap(),
        control_origin: format!("http://{}", provider.server_addr())
            .try_into()
            .unwrap(),
    };
    let mut claims = AccountAuthorization {
        lease: lease(&audience, 1),
        account_id: "owner".to_string().try_into().unwrap(),
        session_id: "platform-session".to_string().try_into().unwrap(),
        membership_id: "member".to_string().try_into().unwrap(),
        display_name: "Shared account".into(),
        capabilities: vec![Capability::UseProduct, Capability::ManageInstance],
    };
    let mode = Arc::new(AtomicUsize::new(0));
    let calls = Arc::new(AtomicUsize::new(0));
    let challenge = Arc::new(std::sync::Mutex::new(String::new()));
    let exchanges = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let worker = {
        let challenge = challenge.clone();
        let exchanges = exchanges.clone();
        let (mode, calls, stop, audience, mut claims) = (
            mode.clone(),
            calls.clone(),
            stop.clone(),
            audience.clone(),
            claims.clone(),
        );
        std::thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                let Some(mut request) = provider.recv_timeout(Duration::from_millis(50)).unwrap()
                else {
                    continue;
                };
                assert!(
                    request
                        .headers()
                        .iter()
                        .any(|h| h.field.equiv("Authorization")
                            && h.value.as_str() == format!("Bearer {}", "ab".repeat(32)))
                );
                let mut bytes = Vec::new();
                request.as_reader().read_to_end(&mut bytes).unwrap();
                let input: alt_json::Value = alt_json::from_slice(&bytes).unwrap();
                assert_eq!(
                    input["audience"],
                    alt_json_serde::to_value(&&audience).unwrap()
                );
                let sequence = calls.fetch_add(1, Ordering::SeqCst) as u64 + 2;
                let (status, body) = match mode.load(Ordering::SeqCst) {
                    3 if request.url().ends_with("/exchange") => {
                        exchanges.fetch_add(1, Ordering::SeqCst);
                        (503, alt_json::json!({}))
                    }
                    1 => (503, alt_json::json!({})),
                    _ if request.url().ends_with("/exchange") => {
                        exchanges.fetch_add(1, Ordering::SeqCst);
                        assert_eq!(input["ticket"], "ff".repeat(32));
                        let verifier = secret(input["verifier"].as_str().unwrap());
                        assert_eq!(
                            golia_platform_contract::pkce_challenge(&verifier),
                            *challenge.lock().unwrap()
                        );
                        claims.lease = lease(&audience, sequence);
                        (
                            200,
                            alt_json::json!({"version":1,"grant":"cd".repeat(32),"account":alt_json_serde::Serde(&claims)}),
                        )
                    }
                    2 if request.url().ends_with("/introspect") => (403, alt_json::json!({})),
                    _ if request.url().ends_with("/service") => (
                        200,
                        alt_json_serde::to_value(&AltServiceAuthorization {
                            lease: lease(&audience, sequence),
                            writes_enabled: false,
                            limits: AltLimits {
                                repositories: Counter(2),
                                stored_bytes: Counter(100_000),
                                monthly_ingress_bytes: Counter(100_000),
                            },
                        })
                        .unwrap(),
                    ),
                    _ => {
                        assert_eq!(input["grant"], "cd".repeat(32));
                        claims.lease = lease(&audience, sequence);
                        (200, alt_json_serde::to_value(&&claims).unwrap())
                    }
                };
                request
                    .respond(
                        Response::from_string(body.to_string())
                            .with_status_code(status)
                            .with_header(
                                Header::from_bytes("Content-Type", "application/json").unwrap(),
                            ),
                    )
                    .unwrap();
            }
        })
    };
    f.client()
        .batch_execute("DROP SCHEMA public CASCADE; CREATE SCHEMA public")
        .unwrap();
    f.spec.deployment_mode = DeploymentMode::Cloud;
    let runtime = Arc::new(
        alt_server::cloud::Runtime::new(audience.clone(), secret(&"ab".repeat(32))).unwrap(),
    );
    f.store = Store::initialize_cloud(
        f.client(),
        ByteStore::open(&f.root.path().join("bytes")).unwrap(),
        f.spec.clone(),
        SigningKey::from_bytes(&[7; 32]),
        limits(),
        &audience,
        runtime.key_digest(),
    )
    .unwrap();
    let token = format!("alt1_{}", "ef".repeat(32));
    let digest = alt_server_store::cloud_credential_digest(&token).unwrap();
    let sealed = alt_cloud_client::GrantSealer::new(audience.clone(), &secret(&"ab".repeat(32)))
        .unwrap()
        .seal(&secret(&"cd".repeat(32)), &digest)
        .unwrap();
    claims.lease.expires_at_unix_ms = Counter(time() + 5000);
    f.store
        .accept_cloud_login(&token, &sealed, &claims, time() + 3_600_000, true)
        .unwrap();
    let assets = f.root.path().join("assets");
    std::fs::create_dir(&assets).unwrap();
    std::fs::write(
        assets.join("index.html"),
        "<!doctype html><title>alt</title>",
    )
    .unwrap();
    let origin = alt_browser_auth::PublicOrigin::parse(audience.origin.as_str()).unwrap();
    let server = Running::start_with_cloud(
        &f,
        Some(alt_server::WebOptions {
            origin: origin.clone(),
            assets: alt_server::WebAssets::load(&assets).unwrap(),
            session_ttl_ms: 60_000,
        }),
        Some(runtime.clone()),
    );
    assert_eq!(server.command("GET", "/v1/capabilities", &token).0, 200);
    assert_eq!(
        server
            .send("GET", "/v1/instance/metrics", Some(&token), b"")
            .status(),
        200
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let cookie = origin
        .set_cookie(&token, 3600)
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let browser = ureq::get(&format!("{}/web/session", server.url))
        .set("Cookie", &cookie)
        .call()
        .unwrap();
    let session: alt_json::Value = crate::database::json::body(browser);
    assert_eq!(session["user"]["display_name"], "Shared account");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "valid persisted leases need no control request"
    );
    // Force the persisted lease near expiry; an outage may only use its remainder.
    f.client().execute("UPDATE alt_cloud_credentials SET claims=jsonb_set(claims,'{lease,expires_at_unix_ms}',to_jsonb($1::text))", &[&(time()+3000).to_string()]).unwrap();
    mode.store(1, Ordering::SeqCst);
    assert_eq!(server.command("GET", "/v1/capabilities", &token).0, 200);
    f.client().execute("UPDATE alt_cloud_credentials SET claims=jsonb_set(claims,'{lease,expires_at_unix_ms}',to_jsonb($1::text))", &[&time().to_string()]).unwrap();
    assert_eq!(server.command("GET", "/v1/capabilities", &token).0, 503);
    assert_eq!(
        server
            .send("GET", "/v1/instance/metrics", Some(&token), b"")
            .status(),
        503
    );
    mode.store(2, Ordering::SeqCst);
    assert_eq!(server.command("GET", "/v1/capabilities", &token).0, 403);
    assert_eq!(
        server
            .send("GET", "/v1/instance/metrics", Some(&token), b"")
            .status(),
        403
    );
    assert!(
        f.store.cloud_refresh(&token).is_err(),
        "definitive refusal persists"
    );
    let wrong = alt_server::cloud::Runtime::new(audience, secret(&"aa".repeat(32))).unwrap();
    assert!(wrong.check(&mut f.store).is_err());
    // Actual browser entry/callback over HTTP, with redirects deliberately not followed.
    mode.store(0, Ordering::SeqCst);
    let agent = ureq::AgentBuilder::new()
        .redirects(0)
        .timeout(Duration::from_secs(10))
        .build();
    let start = agent
        .get(&format!("{}/web/login/start", server.url))
        .call()
        .unwrap();
    assert_eq!(start.status(), 303);
    let pending = start
        .header("Set-Cookie")
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    assert!(
        start
            .header("Set-Cookie")
            .unwrap()
            .contains("HttpOnly; SameSite=Lax")
    );
    let location = url::Url::parse(start.header("Location").unwrap()).unwrap();
    assert_eq!(
        location.origin().ascii_serialization(),
        runtime.audience().control_origin.as_str()
    );
    let args: std::collections::BTreeMap<_, _> = location.query_pairs().into_owned().collect();
    *challenge.lock().unwrap() = args["challenge"].clone();
    let callback = format!(
        "{}/web/login/callback?ticket={}&state={}",
        server.url,
        "ff".repeat(32),
        args["state"]
    );
    let call = |url: &str, cookie: &str| match agent.get(url).set("Cookie", cookie).call() {
        Ok(response) | Err(ureq::Error::Status(_, response)) => response,
        Err(error) => panic!("{error}"),
    };
    assert_eq!(
        call(&callback, &format!("{pending}; {pending}")).status(),
        403
    );
    assert_eq!(
        call(&format!("{callback}&state={}", args["state"]), &pending).status(),
        403
    );
    assert_eq!(exchanges.load(Ordering::SeqCst), 0);
    let completed = call(&callback, &pending);
    assert_eq!(completed.status(), 303);
    assert_eq!(completed.header("Location"), Some("/"));
    let session_cookie = completed
        .all("Set-Cookie")
        .into_iter()
        .find(|c| c.starts_with(origin.cookie_name()))
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    assert_eq!(
        call(&callback, &pending).all("Set-Cookie")[0]
            .split(';')
            .next()
            .unwrap(),
        session_cookie
    );
    assert_eq!(
        exchanges.load(Ordering::SeqCst),
        1,
        "committed result survives a lost response without re-exchange"
    );
    assert_eq!(
        call(&format!("{}/web/session", server.url), &session_cookie).status(),
        200
    );
    // The public CLI gets only a device code. Actual consent requires the
    // existing browser session, exact Origin and CSRF proof.
    use alt_remote_protocol::login::{CLI_CLIENT_ID, CLI_SCOPE, DEVICE_GRANT_TYPE};
    let form_call = |path: &str, body: &[(&str, &str)]| match agent
        .post(&format!("{}{path}", server.url))
        .send_form(body)
    {
        Ok(response) | Err(ureq::Error::Status(_, response)) => response,
        Err(error) => panic!("{error}"),
    };
    let started = form_call(
        "/oauth/device/code",
        &[("client_id", CLI_CLIENT_ID), ("scope", CLI_SCOPE)],
    );
    assert_eq!(started.status(), 200);
    assert_eq!(started.header("Cache-Control"), Some("no-store"));
    let device: alt_json::Value = crate::database::json::body(started);
    assert_eq!(device["interval"], 5);
    let code = device["device_code"].as_str().unwrap();
    let human = device["user_code"].as_str().unwrap();
    let polling = [
        ("client_id", CLI_CLIENT_ID),
        ("grant_type", DEVICE_GRANT_TYPE),
        ("device_code", code),
    ];
    let waiting: alt_json::Value = crate::database::json::body(form_call("/oauth/token", &polling));
    assert_eq!(waiting["error"], "slow_down");
    let current: alt_json::Value = crate::database::json::body(call(
        &format!("{}/web/session", server.url),
        &session_cookie,
    ));
    let decision = alt_json::json!({"user_code":human,"allow":true}).to_string();
    let decide = |proof: &str| match agent
        .post(&format!("{}/web/device", server.url))
        .set("Cookie", &session_cookie)
        .set("Origin", origin.as_str())
        .set("X-Alt-CSRF", proof)
        .set("Content-Type", "application/json")
        .send_string(&decision)
    {
        Ok(response) | Err(ureq::Error::Status(_, response)) => response,
        Err(error) => panic!("{error}"),
    };
    assert_eq!(decide(&"f".repeat(64)).status(), 403);
    assert_eq!(decide(current["csrf"].as_str().unwrap()).status(), 200);
    assert_eq!(
        decide(current["csrf"].as_str().unwrap()).status(),
        200,
        "exact consent retry does not duplicate issuance"
    );
    let issued = form_call("/oauth/token", &polling);
    assert_eq!(issued.status(), 200);
    let issued: alt_json::Value = crate::database::json::body(issued);
    let cli_token = issued["access_token"].as_str().unwrap();
    let repeated: alt_json::Value =
        crate::database::json::body(form_call("/oauth/token", &polling));
    assert_eq!(issued["access_token"], repeated["access_token"]);
    assert_eq!(server.command("GET", "/v1/capabilities", cli_token).0, 200);
    let cookie_as_cli = origin
        .set_cookie(cli_token, 3600)
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    assert_eq!(
        call(&format!("{}/web/session", server.url), &cookie_as_cli).status(),
        401
    );
    let list: alt_json::Value = crate::database::json::body(call(
        &format!("{}/web/client-credentials", server.url),
        &session_cookie,
    ));
    assert_eq!(list.as_array().unwrap().len(), 1);
    let id = list[0]["id"].as_str().unwrap();
    let revoked = agent
        .post(&format!(
            "{}/web/client-credentials/{id}/revoke",
            server.url
        ))
        .set("Cookie", &session_cookie)
        .set("Origin", origin.as_str())
        .set("X-Alt-CSRF", current["csrf"].as_str().unwrap())
        .set("Content-Type", "application/json")
        .send_string("{}")
        .unwrap();
    assert_eq!(revoked.status(), 200);
    assert_eq!(server.command("GET", "/v1/capabilities", cli_token).0, 403);
    let refused: alt_json::Value = crate::database::json::body(form_call("/oauth/token", &polling));
    assert_eq!(refused["error"], "access_denied");
    // A remote exchange with an indeterminate result is never replayed.
    let start = agent
        .get(&format!("{}/web/login/start", server.url))
        .call()
        .unwrap();
    let pending = start
        .header("Set-Cookie")
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let location = url::Url::parse(start.header("Location").unwrap()).unwrap();
    let args: std::collections::BTreeMap<_, _> = location.query_pairs().into_owned().collect();
    let callback = format!(
        "{}/web/login/callback?ticket={}&state={}",
        server.url,
        "ff".repeat(32),
        args["state"]
    );
    mode.store(3, Ordering::SeqCst);
    assert_eq!(call(&callback, &pending).status(), 503);
    assert_eq!(call(&callback, &pending).status(), 503);
    assert_eq!(exchanges.load(Ordering::SeqCst), 2);
    drop(server);
    stop.store(true, Ordering::SeqCst);
    worker.join().unwrap();
}
