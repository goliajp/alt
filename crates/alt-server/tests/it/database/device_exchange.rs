//! A device code that was exchanged and whose token could not be handed over.
use super::*;
use alt_remote_protocol::login::{CLI_CLIENT_ID, CLI_SCOPE, DEVICE_GRANT_TYPE};

const ORIGIN: &str = "https://workbench.example.test";

fn form(server: &Running, path: &str, body: &[(&str, &str)]) -> (u16, alt_json::Value) {
    let request = ureq::post(&format!("{}{path}", server.url)).timeout(Duration::from_secs(30));
    let response = match request.send_form(body) {
        Ok(response) | Err(ureq::Error::Status(_, response)) => response,
        Err(error) => panic!("{error}"),
    };
    (response.status(), json::body(response))
}

/// a code the account `reader` consented to in its browser session
fn approved(server: &Running) -> String {
    let started = form(
        server,
        "/oauth/device/code",
        &[("client_id", CLI_CLIENT_ID), ("scope", CLI_SCOPE)],
    );
    assert_eq!(started.0, 200, "{:?}", started.1);
    let code = started.1["device_code"].as_str().unwrap().to_owned();
    let human = started.1["user_code"].as_str().unwrap().to_owned();
    let credentials = alt_json::json!({
        "login_name": "reader",
        "password": "a private browser acceptance password",
    });
    let signed_in = ureq::post(&format!("{}/web/login", server.url))
        .set("Origin", ORIGIN)
        .set("Content-Type", "application/json")
        .send_bytes(&alt_json::to_vec(&credentials).unwrap())
        .unwrap();
    let cookie = signed_in.header("Set-Cookie").unwrap();
    let cookie = cookie.split(';').next().unwrap().to_owned();
    let session: alt_json::Value = json::body(signed_in);
    let decision = alt_json::json!({"user_code": human, "allow": true}).to_string();
    let decided = ureq::post(&format!("{}/web/device", server.url))
        .set("Cookie", &cookie)
        .set("Origin", ORIGIN)
        .set("X-Alt-CSRF", session["csrf"].as_str().unwrap())
        .set("Content-Type", "application/json")
        .send_string(&decision)
        .unwrap();
    assert_eq!(decided.status(), 200);
    code
}

/// what a device that polls with `code` is told
fn polled(server: &Running, code: &str) -> (u16, alt_json::Value) {
    let polling = [
        ("client_id", CLI_CLIENT_ID),
        ("grant_type", DEVICE_GRANT_TYPE),
        ("device_code", code),
    ];
    let (status, told) = form(server, "/oauth/token", &polling);
    let error = told["error"].clone();
    (status, if status == 200 { told } else { error })
}

/// the tokens of devices that are good, and the codes not yet exchanged
fn left(f: &Fixture) -> (i64, i64) {
    let count = |sql: &str| -> i64 { f.client().query_one(sql, &[]).unwrap().get(0) };
    (
        count(
            "SELECT count(*) FROM alt_credentials c JOIN alt_client_credentials USING(token_digest) WHERE NOT c.revoked",
        ),
        count("SELECT count(*) FROM alt_device_authorizations"),
    )
}

/// The exchange spends the code and then reads the credential it stands for. Where
/// that read fails, asking again with the code gets nothing: the client is told
/// to begin again, not to try again later, and the token nobody was given is
/// revoked, so that it is not among those its account may have.
#[test]
#[ignore = "requires isolated PostgreSQL and loopback HTTP; exercised by browser acceptance and closing gate"]
fn a_code_exchanged_for_a_token_that_was_not_handed_over_says_to_begin_again() {
    let mut f = Fixture::new();
    let server = sign_in_deadline::start(&mut f, |_, _| ());
    let code = approved(&server);
    assert_eq!(left(&f), (1, 1));

    // the account the token is of is held: the exchange spends the code, and its
    // read of the account waits until its statement is ended
    let mut holder = f.client();
    let mut held = holder.transaction().unwrap();
    held.query_one(
        "SELECT 1 FROM alt_web_accounts WHERE actor_id='reader' FOR UPDATE",
        &[],
    )
    .unwrap();
    assert_eq!(polled(&server, &code), (400, "expired_token".into()));
    assert_eq!(left(&f), (0, 0));
    held.commit().unwrap();
    assert_eq!(polled(&server, &code), (400, "invalid_grant".into()));
    let revoked: i64 = f
        .client()
        .query_one(
            "SELECT count(*) FROM alt_instance_audit WHERE action='client.revoke'",
            &[],
        )
        .unwrap()
        .get(0);
    assert_eq!(revoked, 1);
}

/// The time a request has is over while its code is being exchanged: the device
/// is told to begin again, and the worker, which goes on, revokes the token it
/// finds nobody waiting for.
#[test]
#[ignore = "requires isolated PostgreSQL and loopback HTTP; exercised by browser acceptance and closing gate"]
fn a_code_exchanged_past_the_time_of_its_request_leaves_no_token_behind() {
    let mut f = Fixture::new();
    let server = sign_in_deadline::start(&mut f, |factory, options| {
        factory.statement_timeout = Duration::from_secs(60);
        options.work_timeout = Duration::from_secs(2);
    });
    let code = approved(&server);
    let mut holder = f.client();
    let mut held = holder.transaction().unwrap();
    held.query_one(
        "SELECT 1 FROM alt_web_accounts WHERE actor_id='reader' FOR UPDATE",
        &[],
    )
    .unwrap();
    assert_eq!(polled(&server, &code), (400, "expired_token".into()));
    // the code is spent; its token is good until the worker is through
    assert_eq!(left(&f), (1, 0));
    held.commit().unwrap();
    // stopping the service joins the worker
    drop(server);
    assert_eq!(left(&f), (0, 0));
}

/// The time a request has is over before its code is spent: the code is left as
/// it was, the device is told to ask again, and asking again is given the token.
#[test]
#[ignore = "requires isolated PostgreSQL and loopback HTTP; exercised by browser acceptance and closing gate"]
fn a_code_whose_request_left_before_it_was_spent_is_good_for_its_token_still() {
    let mut f = Fixture::new();
    let server = sign_in_deadline::start(&mut f, |factory, options| {
        factory.statement_timeout = Duration::from_secs(60);
        options.work_timeout = Duration::from_secs(2);
    });
    let code = approved(&server);
    // the code is held: the exchange waits to look at it
    let mut holder = f.client();
    let mut held = holder.transaction().unwrap();
    held.query_one("SELECT 1 FROM alt_device_authorizations FOR UPDATE", &[])
        .unwrap();
    let (status, told) = polled(&server, &code);
    assert_eq!((status, told), (503, "temporarily_unavailable".into()));
    held.commit().unwrap();
    // the worker of the request that left finds the code and leaves it; a
    // device that asks while it does waits for the row and is given the token
    let (status, told) = polled(&server, &code);
    assert_eq!(status, 200, "{told:?}");
    assert!(told["access_token"].as_str().is_some_and(|t| t.len() == 69));
    assert_eq!(left(&f), (1, 0));
}
