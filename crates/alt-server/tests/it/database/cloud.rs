use super::*;
use golia_platform_contract::{
    AccountAuthorization, AltLimits, AltServiceAuthorization, Audience, Capability, Counter, Lease,
    Product, Secret,
};
use std::sync::{Arc, atomic::AtomicUsize};
use tiny_http::{Header, Response, Server};

mod held_bodies;
mod platform;
mod unanswered;
pub(super) use platform::{Instance, Platform, instance};

pub(super) fn secret(value: &str) -> Secret {
    value.to_owned().try_into().unwrap()
}
pub(super) fn time() -> u64 {
    future() - 3_600_000
}
pub(super) fn lease(audience: &Audience, sequence: u64) -> Lease {
    // one clock reading: a second one can land in the next millisecond and
    // make the lease longer than the contract allows
    let issued = time();
    Lease {
        version: 1,
        audience: audience.clone(),
        sequence: Counter(sequence),
        revocation_generation: Counter(1),
        issued_at_unix_ms: Counter(issued),
        expires_at_unix_ms: Counter(issued + 60_000),
    }
}

#[test]
#[ignore = "requires isolated PostgreSQL and actual loopback HTTP platform fixture"]
fn cloud_http_refresh_outage_revocation_and_browser_identity() {
    let mut f = Fixture::new();
    let platform = Platform::start(&f);
    let Instance {
        server,
        runtime,
        origin,
        token,
    } = instance(&mut f, &platform, |_| ());
    let (mode, calls, challenge) = (&platform.mode, &platform.calls, &platform.challenge);
    let (exchanges, audience) = (&platform.exchanges, platform.audience.clone());
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
    // the platform is not asked again at once after it gave no answer: until
    // it is, the answer is the one of the outage
    let began = std::time::Instant::now();
    while server.command("GET", "/v1/capabilities", &token).0 == 503 {
        assert!(began.elapsed() < Duration::from_secs(30), "not asked again");
        std::thread::sleep(Duration::from_millis(100));
    }
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
    let location = alt_url::Url::parse(start.header("Location").unwrap()).unwrap();
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
    // the code was exchanged: asked for again, it is no code
    let repeated: alt_json::Value =
        crate::database::json::body(form_call("/oauth/token", &polling));
    assert_eq!(repeated["error"], "invalid_grant");
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
    assert_eq!(refused["error"], "invalid_grant");
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
    let location = alt_url::Url::parse(start.header("Location").unwrap()).unwrap();
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
    platform.end();
}
