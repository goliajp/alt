//! Password sign-in whose request stops waiting before the worker is done.
use super::*;
use alt_browser_auth::PublicOrigin;

const ORIGIN: &str = "https://workbench.example.test";
const PASSWORD: &str = "a private browser acceptance password";

#[test]
#[ignore = "requires isolated PostgreSQL and loopback HTTP; exercised by browser acceptance and closing gate"]
fn sign_in_that_outlives_its_request_is_retryable_and_opens_no_session() {
    let mut f = Fixture::new();
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
    let (factory, mut options) = Running::parts(&f, None, vec![]);
    // one password check takes far longer than this on any machine
    options.work_timeout = Duration::from_millis(1);
    let server = Running::serve(
        factory,
        options,
        Some(alt_server::WebOptions {
            origin,
            assets: alt_server::WebAssets::load(&assets).unwrap(),
            session_ttl_ms: 60_000,
        }),
        false,
    );
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
    let failure: alt_json::Value = json::body(response);
    assert_eq!(failure["code"], "temporarily_unavailable");
    assert_eq!(failure["retry"], "same_request");
    // stopping the service joins the worker, so its password check has finished
    drop(server);
    assert_eq!(
        f.client()
            .query_one("SELECT count(*) FROM alt_web_sessions", &[])
            .unwrap()
            .get::<_, i64>(0),
        0
    );
}
