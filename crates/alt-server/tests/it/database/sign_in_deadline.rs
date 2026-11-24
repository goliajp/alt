//! Password sign-in whose request stops waiting before the worker is done.
use super::*;
use alt_browser_auth::PublicOrigin;

const ORIGIN: &str = "https://workbench.example.test";
const PASSWORD: &str = "a private browser acceptance password";

/// a service with one local user, after `adjust` changed its bounds
pub(super) fn start(
    f: &mut Fixture,
    adjust: impl FnOnce(&mut StoreFactory, &mut HttpOptions),
) -> Running {
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

/// the status and the message of what a sign-in was answered with, how long it
/// took, and after how many seconds it was told to come again
fn answered(server: &Running) -> (u16, String, Duration, Option<u64>) {
    let body = alt_json::to_vec(&alt_json::json!({"login_name":"reader","password":PASSWORD}));
    let began = std::time::Instant::now();
    let response = match ureq::post(&format!("{}/web/login", server.url))
        .timeout(Duration::from_secs(30))
        .set("Origin", ORIGIN)
        .set("Content-Type", "application/json")
        .send_bytes(&body.unwrap())
    {
        Ok(response) | Err(ureq::Error::Status(_, response)) => response,
        Err(error) => panic!("{error}"),
    };
    let (status, taken) = (response.status(), began.elapsed());
    let again = response.header("Retry-After").map(|v| v.parse().unwrap());
    let failure: alt_json::Value = json::body(response);
    let message = failure["message"].as_str().unwrap().to_owned();
    (status, message, taken, again)
}

/// No place to check a password in comes free. A sign-in that is let in waits for
/// one as long as a request may run. One that is refused for the number of
/// sign-ins there have been is refused before it would wait: it takes no place,
/// and keeps nobody who is let in from one.
#[test]
#[ignore = "requires isolated PostgreSQL and loopback HTTP; exercised by browser acceptance and closing gate"]
fn a_sign_in_refused_for_how_many_there_were_waits_for_no_place_to_check_its_password_in() {
    let mut f = Fixture::new();
    let wait = Duration::from_secs(3);
    let server = start(&mut f, |_, options| {
        options.password_places = 0;
        options.work_timeout = wait;
    });
    let (status, message, taken, _) = answered(&server);
    assert_eq!((status, message.as_str()), (503, "Sign-in service is busy"));
    assert!(taken >= wait, "{taken:?}");
    // every count of sign-ins is at what it may be
    let spent = f
        .client()
        .execute("UPDATE alt_login_budgets SET attempts=1000000", &[])
        .unwrap();
    assert!(spent >= 3, "{spent} counts were kept of the sign-in");
    let (status, message, taken, again) = answered(&server);
    assert_eq!(status, 409, "{message}");
    assert!(taken < wait, "{taken:?}");
    // and told when the minute its sign-ins were counted in is over
    assert!(
        again.is_some_and(|seconds| (1..=60).contains(&seconds)),
        "{again:?}"
    );
}
