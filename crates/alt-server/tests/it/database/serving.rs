//! One process serves a database at a time, wherever the processes run.

use super::*;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Mutex};

const SERVED: &str = "another server serves this database";
/// the sessions that hold the lock of the serving process, or wait for it
const SESSIONS: &str = "SELECT pid FROM pg_locks WHERE locktype='advisory' AND (classid::bigint<<32|objid::bigint)=715071909112 AND database=(SELECT oid FROM pg_database WHERE datname=current_database()) AND granted=$1";

fn sessions(f: &Fixture, granted: bool) -> Vec<i32> {
    let rows = f.client().query(SESSIONS, &[&granted]).unwrap();
    rows.iter().map(|row| row.get(0)).collect()
}

/// Waits until `met` says so of the sessions that hold the lock.
fn held_until(f: &Fixture, what: &str, met: impl Fn(&[i32]) -> bool) -> Vec<i32> {
    let until = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        let holding = sessions(f, true);
        if met(&holding) {
            return holding;
        }
        assert!(std::time::Instant::now() < until, "{what}: {holding:?}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn ended(f: &Fixture, session: i32) {
    let ended = "SELECT pg_terminate_backend($1)";
    f.client().query_one(ended, &[&session]).unwrap();
}

/// A second server of a database another one serves does not begin, and the
/// first goes on answering; once the first is gone the next one begins. The
/// lock is a session of the database: where the database ends that session,
/// the server begins another and serves on. Where another process has taken
/// the lock by then, the server serves no longer.
#[test]
#[ignore = "requires isolated PostgreSQL and loopback HTTP; exercised by server acceptance and closing gate"]
fn a_database_is_served_by_one_process_at_a_time() {
    let f = Fixture::new();
    let first = Running::start(&f);
    let held = held_until(&f, "the lock of the first", |held| held.len() == 1);
    assert_eq!(Running::refusal(&f, None), SERVED);
    first.metrics(&f.token);
    assert_eq!(sessions(&f, true), held);
    drop(first);
    held_until(&f, "the lock let go", |held| held.is_empty());

    // served by a process whose end is told here
    let (factory, options) = Running::parts(&f, None, vec![]);
    let (address_tx, address_rx) = std::sync::mpsc::channel();
    let (end_tx, end_rx) = std::sync::mpsc::channel();
    let serving = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let served = runtime.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            address_tx.send(listener.local_addr().unwrap()).unwrap();
            alt_server::serve(listener, factory, options, std::future::pending()).await
        });
        end_tx
            .send(served.map_err(|error| error.to_string()))
            .unwrap();
    });
    let url = format!("http://{}", address_rx.recv().unwrap());
    let answers = || {
        let request = ureq::get(&format!("{url}/v1/instance/metrics"));
        let request = request.set("Authorization", &format!("Bearer {}", f.token));
        match request.timeout(Duration::from_secs(15)).call() {
            Ok(response) => response.status(),
            Err(ureq::Error::Status(status, _)) => status,
            Err(error) => panic!("{error}"),
        }
    };
    let before = held_until(&f, "the lock of the second", |held| held.len() == 1);
    assert_eq!(answers(), 200);
    // its session ended under it: another is begun, and it serves on
    ended(&f, before[0]);
    let again = held_until(&f, "the lock taken again", |held| {
        held.len() == 1 && held != before
    });
    // it serves again once it has opened what it shut while it took the lock
    let until = std::time::Instant::now() + Duration::from_secs(10);
    while answers() == 503 && std::time::Instant::now() < until {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(answers(), 200);
    assert!(end_rx.try_recv().is_err());

    // its session ended, and another process is the one that has the lock next
    let mut other = f.client();
    let taking = std::thread::spawn(move || {
        let taken = "SELECT pg_advisory_lock(715071909112)";
        other.query_one(taken, &[]).unwrap();
        other
    });
    let until = std::time::Instant::now() + Duration::from_secs(60);
    while sessions(&f, false).is_empty() {
        assert!(
            std::time::Instant::now() < until,
            "nobody waits for the lock"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    ended(&f, again[0]);
    let other = taking.join().unwrap();
    let served = end_rx.recv_timeout(Duration::from_secs(60));
    assert_eq!(served.unwrap(), Err(SERVED.to_owned()));
    serving.join().unwrap();
    drop(other);
}

/// A line to the database a test can make go silent: each connection made to
/// it goes on to the socket of the database, and those made before
/// [`Line::silent`] carry nothing more either way, without being closed. Once
/// the line is dropped they are closed.
struct Line {
    address: std::net::SocketAddr,
    /// how many connections were made, and how many of them carry nothing more
    made: Arc<(AtomicUsize, AtomicUsize)>,
    clients: Arc<Mutex<Vec<std::net::TcpStream>>>,
}

impl Line {
    fn to(socket: std::path::PathBuf) -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let made = Arc::new((AtomicUsize::new(0), AtomicUsize::new(0)));
        let clients = Arc::new(Mutex::new(Vec::new()));
        let (counts, kept) = (made.clone(), clients.clone());
        std::thread::spawn(move || {
            for client in listener.incoming() {
                let Ok(client) = client else { return };
                let number = counts.0.fetch_add(1, SeqCst);
                let database = std::os::unix::net::UnixStream::connect(&socket).unwrap();
                kept.lock().unwrap().push(client.try_clone().unwrap());
                let ends: [(Box<dyn Read + Send>, Box<dyn Write + Send>); 2] = [
                    (
                        Box::new(client.try_clone().unwrap()),
                        Box::new(database.try_clone().unwrap()),
                    ),
                    (Box::new(database), Box::new(client)),
                ];
                for (mut from, mut to) in ends {
                    let counts = counts.clone();
                    std::thread::spawn(move || {
                        let mut buffer = vec![0; 64 << 10];
                        while let Ok(read @ 1..) = from.read(&mut buffer) {
                            if counts.1.load(SeqCst) > number {
                                return;
                            }
                            if to.write_all(&buffer[..read]).is_err() {
                                return;
                            }
                        }
                    });
                }
            }
        });
        Line {
            address,
            made,
            clients,
        }
    }

    fn silent(&self) {
        self.made.1.store(self.made.0.load(SeqCst), SeqCst);
    }
}

impl Drop for Line {
    fn drop(&mut self) {
        for client in self.clients.lock().unwrap().iter() {
            let _ = client.shutdown(std::net::Shutdown::Both);
        }
    }
}

/// The server reaches its database over a line that goes silent, and the
/// database ends the session that holds the lock, as it does one it hears
/// nothing of. The server hears nothing more of that session either: it
/// gives up on it once its answer is late, begins another and takes the lock
/// again, and a second server is turned away.
#[test]
#[ignore = "requires isolated PostgreSQL and loopback HTTP; exercised by server acceptance and closing gate"]
fn a_server_whose_session_goes_silent_takes_the_lock_again_with_another() {
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
    // the server has begun: it answers once it holds the lock and is done with its session
    let token = f.token.clone();
    let answered = server.send("GET", "/v1/capabilities", Some(&token), b"");
    assert_eq!(answered.status(), 200);
    let before = held_until(&f, "the lock of the server", |held| held.len() == 1);
    line.silent();
    ended(&f, before[0]);
    held_until(&f, "the lock taken again", |held| {
        held.len() == 1 && held != before
    });
    let second = Running::parts(&f, None, vec![]).0.open().unwrap();
    let refused = second.serve_alone(Duration::from_secs(2)).err().unwrap();
    assert!(
        matches!(refused, alt_server_store::Error::Precondition(SERVED)),
        "{refused}"
    );
    drop((line, server));
}

/// The server reaches its database over a line that goes silent; the database
/// ends the session that holds the lock, and another process takes the lock
/// before the server asks for it again. From the moment the server finds its
/// session gone it serves nothing: it answers that it takes its lock again,
/// and nothing is served after that, until it ends, told another serves.
#[test]
#[ignore = "requires isolated PostgreSQL and loopback HTTP; exercised by server acceptance and closing gate"]
fn a_server_that_lost_its_session_serves_nothing_until_it_has_the_lock_again() {
    let f = Fixture::new();
    let admin: alt_pg::Config = f.admin_dsn.parse().unwrap();
    let Some(alt_pg::Host::Unix(directory)) = admin.host() else {
        panic!("the test database is reached on a socket of the host");
    };
    let socket = directory.join(format!(".s.PGSQL.{}", admin.port().unwrap_or(5432)));
    let (user, database) = (admin.user().unwrap().to_owned(), f.database.clone());
    let line = Line::to(socket);
    let port = line.address.port();
    let (mut factory, options) = Running::parts(&f, None, vec![]);
    let reached = format!("host=127.0.0.1 port={port} user={user} dbname={database}");
    factory.database = reached.parse().unwrap();
    let (address_tx, address_rx) = std::sync::mpsc::channel();
    let (end_tx, end_rx) = std::sync::mpsc::channel();
    let serving = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let served = runtime.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            address_tx.send(listener.local_addr().unwrap()).unwrap();
            alt_server::serve(listener, factory, options, std::future::pending()).await
        });
        end_tx
            .send(served.map_err(|error| error.to_string()))
            .unwrap();
    });
    let url = format!("http://{}", address_rx.recv().unwrap());
    // a status, and whether it says the lock is taken again; none once it ends
    let answer = || -> Option<(u16, bool)> {
        let request = ureq::get(&format!("{url}/v1/instance/metrics"))
            .set("Authorization", &format!("Bearer {}", f.token))
            .timeout(Duration::from_secs(5));
        match request.call() {
            Ok(response) => Some((response.status(), false)),
            Err(ureq::Error::Status(status, response)) => {
                let said = response.into_string().unwrap_or_default();
                Some((
                    status,
                    said.contains("taking its lock of the database again"),
                ))
            }
            Err(ureq::Error::Transport(error)) => match error.kind() {
                ureq::ErrorKind::ConnectionFailed => None,
                // one that hangs on a connection the line keeps silent
                _ => Some((0, false)),
            },
        }
    };
    let before = held_until(&f, "the lock of the server", |held| held.len() == 1);
    assert_eq!(answer(), Some((200, false)));
    line.silent();
    ended(&f, before[0]);
    let mut other = f.client();
    other
        .query_one("SELECT pg_advisory_lock(715071909112)", &[])
        .unwrap();
    let mut answers = Vec::new();
    let until = std::time::Instant::now() + Duration::from_secs(240);
    while let Some(answered) = answer() {
        answers.push(answered);
        assert!(std::time::Instant::now() < until, "{answers:?}");
        std::thread::sleep(Duration::from_millis(200));
    }
    let first = answers
        .iter()
        .position(|(status, again)| *status == 503 && *again);
    let first = first.unwrap_or_else(|| panic!("never told the lock is taken again: {answers:?}"));
    assert!(
        answers[first..].iter().all(|(status, _)| *status != 200),
        "{answers:?}"
    );
    let served = end_rx.recv_timeout(Duration::from_secs(60));
    assert_eq!(served.unwrap(), Err(SERVED.to_owned()));
    serving.join().unwrap();
    drop((other, line));
}
