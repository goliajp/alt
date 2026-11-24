//! One process serves a database at a time, wherever the processes run.

use super::*;

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
        request
            .timeout(Duration::from_secs(15))
            .call()
            .unwrap()
            .status()
    };
    let before = held_until(&f, "the lock of the second", |held| held.len() == 1);
    assert_eq!(answers(), 200);
    // its session ended under it: another is begun, and it serves on
    ended(&f, before[0]);
    let again = held_until(&f, "the lock taken again", |held| {
        held.len() == 1 && held != before
    });
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
