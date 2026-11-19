//! Password sign-in whose request stops waiting before the worker is done.
use super::*;
use alt_browser_auth::PublicOrigin;

const ORIGIN: &str = "https://workbench.example.test";
const PASSWORD: &str = "a private browser acceptance password";

/// a service with one local user, after `adjust` changed its bounds
fn start(f: &mut Fixture, adjust: impl FnOnce(&mut StoreFactory, &mut HttpOptions)) -> Running {
    f.store
        .create_local_user(&f.credential, "reader", "reader", "reader", PASSWORD, false)
        .unwrap();
    let origin = PublicOrigin::parse(ORIGIN).unwrap();
    f.store.enable_browser(&f.credential, &origin).unwrap();
    let assets = f.root.path().join("assets");
    std::fs::create_dir_all(&assets).unwrap();
    std::fs::write(
        assets.join("index.html"),
        "<!doctype html><title>alt</title>",
    )
    .unwrap();
    let (mut factory, mut options) = Running::parts(f, None, vec![]);
    adjust(&mut factory, &mut options);
    Running::serve(
        factory,
        options,
        Some(alt_server::WebOptions {
            origin,
            assets: alt_server::WebAssets::load(&assets).unwrap(),
            session_ttl_ms: 60_000,
        }),
        false,
    )
}
/// the failure of a sign-in the service answered with 503
fn refused(server: &Running) -> alt_json::Value {
    let body = alt_json::to_vec(&alt_json::json!({"login_name":"reader","password":PASSWORD}));
    let response = match ureq::post(&format!("{}/web/login", server.url))
        .timeout(Duration::from_secs(15))
        .set("Origin", ORIGIN)
        .set("Content-Type", "application/json")
        .send_bytes(&body.unwrap())
    {
        Err(ureq::Error::Status(503, response)) => response,
        other => panic!("sign-in past its deadline: {other:?}"),
    };
    assert!(response.header("Set-Cookie").is_none());
    json::body(response)
}
fn sessions(f: &Fixture) -> i64 {
    f.client()
        .query_one("SELECT count(*) FROM alt_web_sessions", &[])
        .unwrap()
        .get(0)
}

#[test]
#[ignore = "requires isolated PostgreSQL and loopback HTTP; exercised by browser acceptance and closing gate"]
fn sign_in_that_outlives_its_request_is_retryable_and_opens_no_session() {
    let mut f = Fixture::new();
    // one password check takes far longer than this on any machine
    let server = start(&mut f, |_, options| {
        options.work_timeout = Duration::from_millis(1)
    });
    let failure = refused(&server);
    assert_eq!(failure["code"], "temporarily_unavailable");
    assert_eq!(failure["retry"], "same_request");
    // stopping the service joins the worker, so its password check has finished
    drop(server);
    assert_eq!(sessions(&f), 0);
}

#[test]
#[ignore = "requires isolated PostgreSQL and loopback HTTP; exercised by browser acceptance and closing gate"]
fn sign_in_whose_session_write_outlives_its_request_reports_an_unknown_outcome() {
    let mut f = Fixture::new();
    // the session write waits for the account row for as long as this test holds it
    let server = start(&mut f, |factory, options| {
        factory.statement_timeout = Duration::from_secs(60);
        options.work_timeout = Duration::from_secs(5);
    });
    let mut holder = f.client();
    let mut held = holder.transaction().unwrap();
    held.query_one(
        "SELECT 1 FROM alt_web_accounts WHERE actor_id='reader' FOR UPDATE",
        &[],
    )
    .unwrap();
    let failure = refused(&server);
    assert_eq!(failure["code"], "outcome_unknown");
    assert_eq!(failure["retry"], "query_outcome");
    assert_eq!(sessions(&f), 0);
    held.commit().unwrap();
    // the write the request stopped waiting for still completes
    drop(server);
    assert_eq!(sessions(&f), 1);
}
