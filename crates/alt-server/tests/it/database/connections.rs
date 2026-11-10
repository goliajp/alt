use super::*;
use std::collections::BTreeSet;

fn backends(client: &mut Client, database: &str) -> BTreeSet<i32> {
    client
        .query(
            "SELECT pid FROM pg_stat_activity WHERE datname=$1",
            &[&database],
        )
        .unwrap()
        .into_iter()
        .map(|row| row.get(0))
        .collect()
}

#[test]
#[ignore = "requires isolated PostgreSQL and HTTP; cancellation must close retained synchronous clients outside async tasks"]
fn cancelled_http_server_releases_retained_database_connections() {
    let f = Fixture::new();
    let mut client = f.client();
    let existing = backends(&mut client, &f.database);
    let server = Running::start_with_shutdown(&f, None, None, vec![], true);
    assert_eq!(server.send("GET", "/healthz", None, b"").status(), 200);
    assert_eq!(server.send("GET", "/healthz", None, b"").status(), 200);
    assert_eq!(
        backends(&mut client, &f.database)
            .difference(&existing)
            .count(),
        1
    );
    drop(server);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while backends(&mut client, &f.database) != existing {
        assert!(
            std::time::Instant::now() < deadline,
            "cancelled server retained connections"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
#[ignore = "requires isolated PostgreSQL and HTTP; verifies reuse without stale schema, instance or credential authority"]
fn reused_http_connections_recheck_current_state_and_discard_closed_backends() {
    let f = Fixture::new();
    let mut client = f.client();
    let existing = backends(&mut client, &f.database);
    let server = Running::start(&f);
    let health = || server.send("GET", "/healthz", None, b"").status();
    assert_eq!(health(), 200);
    let retained: BTreeSet<_> = backends(&mut client, &f.database)
        .difference(&existing)
        .copied()
        .collect();
    assert_eq!(retained.len(), 1);
    for _ in 0..8 {
        assert_eq!(health(), 200);
        assert_eq!(
            backends(&mut client, &f.database)
                .difference(&existing)
                .copied()
                .collect::<BTreeSet<_>>(),
            retained
        );
    }
    let checksum: String = client
        .query_one("SELECT checksum FROM alt_schema WHERE version=1", &[])
        .unwrap()
        .get(0);
    client
        .execute(
            "UPDATE alt_schema SET checksum='altered' WHERE version=1",
            &[],
        )
        .unwrap();
    assert_eq!(health(), 503);
    client
        .execute(
            "UPDATE alt_schema SET checksum=$1 WHERE version=1",
            &[&checksum],
        )
        .unwrap();
    assert_eq!(health(), 200);
    client
        .execute("UPDATE alt_instance SET runtime_generation=2", &[])
        .unwrap();
    assert_eq!(health(), 503);
    client
        .execute("UPDATE alt_instance SET runtime_generation=1", &[])
        .unwrap();
    assert_eq!(health(), 200);
    let path = "/v1/repositories/repository";
    assert_eq!(server.send("GET", path, Some(&f.token), b"").status(), 200);
    client
        .execute("UPDATE alt_credentials SET revoked=true", &[])
        .unwrap();
    assert_eq!(server.send("GET", path, Some(&f.token), b"").status(), 403);
    client
        .execute("UPDATE alt_credentials SET revoked=false", &[])
        .unwrap();
    assert_eq!(server.send("GET", path, Some(&f.token), b"").status(), 200);

    let live = backends(&mut client, &f.database);
    let owned: Vec<_> = live.difference(&existing).copied().collect();
    assert_eq!(owned.len(), 1);
    // Kill only the backend identified in this isolated database. The failed
    // checkout must surface once; the next request may open a new connection.
    assert!(
        client
            .query_one("SELECT pg_terminate_backend($1)", &[&owned[0]])
            .unwrap()
            .get::<_, bool>(0)
    );
    assert_eq!(health(), 503);
    assert_eq!(health(), 200);

    let start = std::sync::Arc::new(std::sync::Barrier::new(12));
    let threads: Vec<_> = (0..12)
        .map(|_| {
            let start = start.clone();
            let url = format!("{}/healthz", server.url);
            std::thread::spawn(move || {
                start.wait();
                match ureq::get(&url).timeout(Duration::from_secs(15)).call() {
                    Ok(response) => assert_eq!(response.status(), 200),
                    Err(ureq::Error::Status(status, _)) => assert_eq!(status, 503),
                    Err(error) => panic!("health transport: {error}"),
                }
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
    assert_eq!(health(), 200);
    let count = backends(&mut client, &f.database)
        .difference(&existing)
        .count();
    assert!((1..=4).contains(&count), "idle connection count {count}");
    drop(server);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while backends(&mut client, &f.database) != existing {
        assert!(
            std::time::Instant::now() < deadline,
            "HTTP connections outlived server"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
