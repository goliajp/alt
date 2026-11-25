//! What a request keeps while it waits: a migration whose body is whole its place
//! among the bodies of its caller, until it has a worker; and any request its
//! connection, until it is answered.
use super::bodies::{answer, connect, gone_on};
use super::*;
use crate::database::sign_in_deadline::start;
use std::io::Write;

const ORIGIN: &str = "https://workbench.example.test";
const PASSWORD: &str = "a private browser acceptance password";

/// what a sign-in of the account that may migrate is answered with
fn sign_in(server: &Running) -> ureq::Response {
    let body = alt_json::json!({"login_name": "browser", "password": PASSWORD});
    let asked = ureq::post(&format!("{}/web/login", server.url))
        .timeout(Duration::from_secs(30))
        .set("Origin", ORIGIN)
        .set("Content-Type", "application/json");
    match asked.send_bytes(&alt_json::to_vec(&body).unwrap()) {
        Ok(response) | Err(ureq::Error::Status(_, response)) => response,
        Err(error) => panic!("{error}"),
    }
}

#[test]
#[ignore = "requires isolated PostgreSQL and loopback HTTP; exercised by browser acceptance and closing gate"]
fn a_migration_whose_body_is_whole_keeps_its_place_until_it_has_a_worker() {
    let mut f = Fixture::new();
    let (store, admin) = (&mut f.store, &f.credential);
    store
        .create_local_user(admin, "browser", "browser", "browser", PASSWORD, true)
        .unwrap();
    store
        .set_grants(admin, "repository", "browser", true, true, &grants())
        .unwrap();
    let server = start(&mut f, |factory, options| {
        factory.statement_timeout = Duration::from_secs(60);
        options.workers = 1;
        options.receiving_each = 1;
        options.work_timeout = Duration::from_secs(20);
    });
    let signed_in = sign_in(&server);
    assert_eq!(signed_in.status(), 200);
    let cookie = signed_in.header("Set-Cookie").unwrap();
    let cookie = cookie.split(';').next().unwrap().to_owned();
    let session: alt_json::Value = json::body(signed_in);
    let csrf = session["csrf"].as_str().unwrap();

    // a migration request is authorized and told to send its body: it has the
    // one place its address has among the bodies being received
    let (mut socket, mut reader) = connect(&server);
    let head = format!(
        "POST /web/repositories/repository/epochs/1/portable/inspect HTTP/1.1\r\nHost: alt\r\nCookie: {cookie}\r\nOrigin: {ORIGIN}\r\nX-Alt-CSRF: {csrf}\r\nContent-Type: application/json\r\nExpect: 100-continue\r\nContent-Length: 64\r\n\r\n"
    );
    socket.write_all(head.as_bytes()).unwrap();
    gone_on(&mut reader);

    // the one worker is taken by a request that waits for a row the test holds
    let mut holder = f.client();
    let mut held = holder.transaction().unwrap();
    let row = "SELECT 1 FROM alt_repositories WHERE id='repository' FOR UPDATE";
    held.query_one(row, &[]).unwrap();
    let payload = Payload::new("other", None);
    let begin = alt_json::to_vec(&alt_server::BeginRequest {
        publication: payload.publication.clone(),
        manifest: payload.manifest.clone(),
    })
    .unwrap();
    let (url, token) = (server.url.clone(), f.token.clone());
    let other = std::thread::spawn(move || {
        let path = "/v1/repositories/repository/epochs/1/requests";
        let request = ureq::post(&format!("{url}{path}"))
            .set("Authorization", &format!("Bearer {token}"))
            .set("Content-Type", "application/json");
        let _ = request.timeout(Duration::from_secs(90)).send_bytes(&begin);
    });
    let waits = "SELECT count(*) FROM pg_stat_activity WHERE wait_event_type='Lock' AND datname=current_database()";
    let until = std::time::Instant::now() + Duration::from_secs(30);
    while f.client().query_one(waits, &[]).unwrap().get::<_, i64>(0) == 0 {
        assert!(
            std::time::Instant::now() < until,
            "the worker is never taken"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    // its body is whole, and no worker is free: it waits with its place
    socket.write_all(&[b' '; 64]).unwrap();
    std::thread::sleep(Duration::from_millis(500));
    for _ in 0..3 {
        let refused = sign_in(&server);
        assert_eq!(refused.status(), 429, "{}", refused.into_string().unwrap());
    }

    held.commit().unwrap();
    other.join().unwrap();
    let (status, _) = answer(&mut reader);
    assert!(status.starts_with("HTTP/1.1 4"), "{status}");
    // the place is given back with the worker
    assert_ne!(sign_in(&server).status(), 429);
}

/// A request is answered after longer than a connection is given when nothing
/// moves on it: the connection is there for the answer.
#[test]
#[ignore = "requires isolated PostgreSQL and loopback HTTP; exercised by browser acceptance and closing gate"]
fn a_connection_is_not_over_while_a_request_of_it_waits_for_its_answer() {
    let mut f = Fixture::new();
    let server = start(&mut f, |factory, options| {
        factory.statement_timeout = Duration::from_secs(60);
        options.connection_timeout = Duration::from_secs(1);
        options.work_timeout = Duration::from_secs(30);
    });
    // the request writes to the repository, and waits for the row the test holds
    let mut holder = f.client();
    let mut held = holder.transaction().unwrap();
    let row = "SELECT 1 FROM alt_repositories WHERE id='repository' FOR UPDATE";
    held.query_one(row, &[]).unwrap();
    let payload = Payload::new("waited", None);
    let begin = alt_json::to_vec(&alt_server::BeginRequest {
        publication: payload.publication.clone(),
        manifest: payload.manifest.clone(),
    })
    .unwrap();
    let (url, token) = (server.url.clone(), f.token.clone());
    let asking = std::thread::spawn(move || {
        let path = "/v1/repositories/repository/epochs/1/requests";
        let request = ureq::post(&format!("{url}{path}"))
            .set("Authorization", &format!("Bearer {token}"))
            .set("Content-Type", "application/json");
        let began = std::time::Instant::now();
        let answered = request.timeout(Duration::from_secs(60)).send_bytes(&begin);
        (answered.map(|response| response.status()), began.elapsed())
    });
    std::thread::sleep(Duration::from_secs(4));
    held.commit().unwrap();
    let (answered, taken) = asking.join().unwrap();
    assert!(taken >= Duration::from_secs(3), "{taken:?}");
    assert!(
        matches!(answered, Ok(200 | 201)),
        "after {taken:?}: {answered:?}"
    );
}
