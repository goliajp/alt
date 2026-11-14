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

/// sequential scans, index scans and rows read sequentially, per table, as reported so far
fn scans(client: &mut Client) -> Vec<(String, i64, i64, i64)> {
    client
        .query(
            "SELECT relname::text,seq_scan,idx_scan,seq_tup_read FROM pg_stat_user_tables WHERE relname IN ('alt_refs','alt_repositories','alt_grants') ORDER BY relname",
            &[],
        )
        .unwrap()
        .into_iter()
        .map(|row| (row.get(0), row.get(1), row.get::<_, Option<i64>>(2).unwrap_or(0), row.get(3)))
        .collect()
}

#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; request plans must not depend on when statistics were collected"]
fn requests_never_scan_authority_tables_sequentially_after_statistics_of_small_tables() {
    use crate::database::git::{identity, ok, text};
    let mut f = Fixture::new();
    let mut client = f.client();
    // a backend reports its counters at the end of a transaction once a second
    // has passed since its last report; the fixture's own connection has scanned
    std::thread::sleep(Duration::from_millis(1100));
    f.store.authenticate(&f.token).unwrap();
    let existing = backends(&mut client, &f.database);
    let before = scans(&mut client);

    let server = Running::start(&f);
    let temp = alt_os::temp::tempdir().unwrap();
    let first = temp.path().join("first");
    std::fs::create_dir(&first).unwrap();
    ok(&first, &f.token, &["init", "-q", "--initial-branch=main"]);
    identity(&first, &f.token);
    std::fs::write(first.join("file"), b"one\n").unwrap();
    ok(&first, &f.token, &["add", "file"]);
    ok(&first, &f.token, &["commit", "-qm", "one"]);
    let head = text(ok(&first, &f.token, &["rev-parse", "HEAD"]));
    let url = format!("{}/git/repository", server.url);
    ok(&first, &f.token, &["push", "-q", &url, "refs/heads/main"]);

    // statistics describe one-page tables, and every plan cached from here on is made from them
    client
        .batch_execute("ANALYZE alt_refs; ANALYZE alt_repositories; ANALYZE alt_grants")
        .unwrap();
    for _ in 0..8 {
        ok(&first, &f.token, &["ls-remote", "--refs", &url]);
    }

    let mut packed = String::from("# pack-refs with: peeled fully-peeled sorted \n");
    for group in 0..10 {
        for index in 0..100 {
            packed.push_str(&format!("{head} refs/heads/g{group:02}/t{index:02}\n"));
        }
    }
    packed.push_str(&format!("{head} refs/heads/main\n"));
    std::fs::write(first.join(".git/packed-refs"), packed).unwrap();
    std::fs::remove_file(first.join(".git/refs/heads/main")).unwrap();
    for group in 0..10 {
        let refspec = format!("refs/heads/g{group:02}/*:refs/heads/g{group:02}/*");
        ok(&first, &f.token, &["push", "-q", &url, &refspec]);
    }
    let pages: i64 = client
        .query_one("SELECT pg_relation_size('alt_refs')/8192", &[])
        .unwrap()
        .get(0);
    assert!(pages > 1, "alt_refs still fits in one page");

    for _ in 0..8 {
        let listed = text(ok(&first, &f.token, &["ls-remote", "--refs", &url]));
        assert_eq!(listed.lines().count(), 1001);
    }
    ok(temp.path(), &f.token, &["clone", "--quiet", &url, "second"]);
    std::fs::write(first.join("file"), b"two\n").unwrap();
    ok(&first, &f.token, &["commit", "-qam", "two"]);
    ok(
        &first,
        &f.token,
        &["push", "-q", &url, "HEAD:refs/heads/main"],
    );
    ok(
        &temp.path().join("second"),
        &f.token,
        &["fetch", "-q", &url],
    );

    // a backend reports whatever it still holds when it exits
    drop(server);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while backends(&mut client, &f.database) != existing {
        assert!(
            std::time::Instant::now() < deadline,
            "HTTP connections outlived server"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let after = scans(&mut client);
    assert_eq!(before.len(), 3);
    let sequential = |scans: &[(String, i64, i64, i64)]| -> Vec<(String, i64, i64)> {
        scans
            .iter()
            .map(|(table, scans, _, rows)| (table.clone(), *scans, *rows))
            .collect()
    };
    assert_eq!(
        sequential(&before),
        sequential(&after),
        "requests scanned sequentially (table, scans, rows read)"
    );
    // the counters do move when these connections read the tables
    for (before, after) in before.iter().zip(&after) {
        assert!(after.2 > before.2, "no index scan of {} reported", after.0);
    }
}
