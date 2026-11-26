//! A session of the lock that is slow to answer is not one that is gone.

use super::serving::{held_until, sessions};
use super::serving_line::Line;
use super::*;

/// The server reaches its database over a line that holds up its session of
/// the lock, for longer than the server waits for an answer before it asks
/// another session who holds the lock, and for less than the database takes
/// to end a session it hears nothing of. The database tells that the session
/// holds the lock still: the server answers throughout, keeps the session,
/// and serves on once the line lets the session go on.
#[test]
#[ignore = "requires isolated PostgreSQL and loopback HTTP; exercised by server acceptance and closing gate"]
fn a_server_whose_session_is_slow_to_answer_serves_on_and_keeps_its_lock() {
    let mut f = Fixture::new();
    let admin: alt_pg::Config = f.admin_dsn.parse().unwrap();
    let Some(alt_pg::Host::Unix(directory)) = admin.host() else {
        panic!("the test database is reached on a socket of the host");
    };
    let socket = directory.join(format!(".s.PGSQL.{}", admin.port().unwrap_or(5432)));
    let (user, database) = (admin.user().unwrap().to_owned(), f.database.clone());
    // the line is dropped first, and the server can end what it reads on it
    let server;
    let line = Line::to(socket);
    let port = line.address.port();
    server = super::sign_in_deadline::start(&mut f, |factory, _| {
        let reached = format!("host=127.0.0.1 port={port} user={user} dbname={database}");
        factory.database = reached.parse().unwrap();
    });
    let token = f.token.clone();
    let metrics = || {
        let answered = server.send("GET", "/v1/instance/metrics", Some(&token), b"");
        answered.status()
    };
    assert_eq!(metrics(), 200);
    let before = held_until(&f, "the lock of the server", |held| held.len() == 1);

    line.held_up(true);
    let until = std::time::Instant::now() + Duration::from_secs(25);
    let mut answers = Vec::new();
    while std::time::Instant::now() < until {
        answers.push(metrics());
        std::thread::sleep(Duration::from_millis(500));
    }
    line.held_up(false);
    assert!(answers.iter().all(|status| *status == 200), "{answers:?}");
    assert_eq!(sessions(&f, true), before);
    // it asks the session again, which answers now, and keeps it
    std::thread::sleep(Duration::from_secs(12));
    assert_eq!(metrics(), 200);
    assert_eq!(sessions(&f, true), before);
    drop((line, server));
}
