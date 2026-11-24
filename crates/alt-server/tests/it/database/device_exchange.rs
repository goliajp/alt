//! A device code that was exchanged and whose token could not be handed over.
use super::*;

const ORIGIN: &str = "https://workbench.example.test";

fn form(server: &Running, path: &str, body: &[(&str, &str)]) -> (u16, alt_json::Value) {
    let request = ureq::post(&format!("{}{path}", server.url)).timeout(Duration::from_secs(30));
    let response = match request.send_form(body) {
        Ok(response) | Err(ureq::Error::Status(_, response)) => response,
        Err(error) => panic!("{error}"),
    };
    (response.status(), json::body(response))
}

/// The exchange spends the code and then reads the credential it stands for. Where
/// that read fails, asking again with the code gets nothing: the client is told
/// to begin again, not to try again later.
#[test]
#[ignore = "requires isolated PostgreSQL and loopback HTTP; exercised by browser acceptance and closing gate"]
fn a_code_exchanged_for_a_token_that_was_not_handed_over_says_to_begin_again() {
    use alt_remote_protocol::login::{CLI_CLIENT_ID, CLI_SCOPE, DEVICE_GRANT_TYPE};
    let mut f = Fixture::new();
    let server = sign_in_deadline::start(&mut f, |_, _| ());
    let started = form(
        &server,
        "/oauth/device/code",
        &[("client_id", CLI_CLIENT_ID), ("scope", CLI_SCOPE)],
    );
    assert_eq!(started.0, 200, "{:?}", started.1);
    let code = started.1["device_code"].as_str().unwrap().to_owned();
    let human = started.1["user_code"].as_str().unwrap().to_owned();

    // the account consents in its browser session
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

    // the row of the credential the code stands for is held: the exchange spends
    // the code, and its read of the credential waits until its statement is ended
    let mut holder = f.client();
    let digest: String = holder
        .query_one(
            "SELECT credential_digest FROM alt_device_authorizations",
            &[],
        )
        .unwrap()
        .get(0);
    let mut held = holder.transaction().unwrap();
    held.query_one(
        "SELECT 1 FROM alt_credentials WHERE token_digest=$1 FOR UPDATE",
        &[&digest],
    )
    .unwrap();
    let polling = [
        ("client_id", CLI_CLIENT_ID),
        ("grant_type", DEVICE_GRANT_TYPE),
        ("device_code", code.as_str()),
    ];
    let (status, failure) = form(&server, "/oauth/token", &polling);
    assert_eq!(
        (status, &failure["error"]),
        (400, &"expired_token".into()),
        "{failure:?}"
    );
    held.commit().unwrap();

    let (status, failure) = form(&server, "/oauth/token", &polling);
    assert_eq!(
        (status, &failure["error"]),
        (400, &"invalid_grant".into()),
        "{failure:?}"
    );
    // the credential nobody holds is there until it is removed with what is over
    let left: i64 = holder
        .query_one(
            "SELECT count(*) FROM alt_credentials WHERE token_digest=$1 AND NOT revoked",
            &[&digest],
        )
        .unwrap()
        .get(0);
    assert_eq!(left, 1);
}
