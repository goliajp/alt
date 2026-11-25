//! What a server does of itself: the requests an upgrade left without the moment
//! they ended are given it as the server runs, and by the server started after
//! it; and what a server writes for a time is on the disk of its bytes.
use crate::database::{Duration, Fixture, Payload, Running};

fn without_moment(f: &Fixture) -> i64 {
    let waiting = "SELECT count(*) FROM alt_requests WHERE settled_at IS NULL AND state='expired'";
    f.client().query_one(waiting, &[]).unwrap().get(0)
}
fn given(f: &Fixture) {
    let until = std::time::Instant::now() + Duration::from_secs(30);
    while without_moment(f) != 0 {
        assert!(std::time::Instant::now() < until);
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
#[ignore = "requires isolated PostgreSQL and loopback HTTP; exercised by server acceptance and closing gate"]
fn a_running_server_gives_the_requests_that_ended_before_it_their_moment() {
    let mut f = Fixture::new();
    for request in ["one", "two", "three"] {
        let payload = Payload::new(request, None);
        (f.store)
            .begin(
                &f.credential,
                &payload.publication,
                &payload.manifest,
                60_000,
            )
            .unwrap();
    }
    f.client()
        .batch_execute("UPDATE alt_requests SET expires_at=0")
        .unwrap();
    assert_eq!(f.store.expire_requests(100, &mut |_, _| ()).unwrap(), 3);
    // as the upgrade leaves the requests that were over before it
    let as_upgraded = "UPDATE alt_requests SET settled_at=NULL";
    f.client().batch_execute(as_upgraded).unwrap();
    assert_eq!(without_moment(&f), 3);

    let serve = |f: &Fixture| {
        let (factory, mut options) = Running::parts(f, None, vec![]);
        options.expiry_interval = Duration::from_millis(200);
        Running::serve(factory, options, None, false)
    };
    let server = serve(&f);
    given(&f);
    drop(server);
    f.client().batch_execute(as_upgraded).unwrap();
    let _server = serve(&f);
    given(&f);
}

/// The server that starts makes `temporary` under the directory of its bytes its
/// own alone, removes what the server before it left there, and has the files of
/// bases under it.
#[test]
#[ignore = "requires isolated PostgreSQL and real operator subprocesses; exercised by closing gate"]
fn a_server_that_starts_has_what_it_writes_for_a_time_under_the_directory_of_its_bytes() {
    use std::os::unix::fs::PermissionsExt;
    let db = super::super::OperatorDb::new();
    let (store, _, _) = super::super::maintenance::prepare(&db);
    drop(store);
    let temporary = db.root.path().join("bytes/temporary");
    std::fs::create_dir_all(temporary.join("left")).unwrap();
    std::fs::write(temporary.join("left/body"), b"left").unwrap();
    std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o755)).unwrap();

    let (_server, url) = super::super::change_links::start(&db);
    let ready = ureq::get(&format!("{url}/healthz")).timeout(Duration::from_secs(5));
    assert_eq!(ready.call().unwrap().status(), 200);
    let mode = |path: &std::path::Path| path.metadata().unwrap().permissions().mode() & 0o777;
    let within: Vec<_> = (std::fs::read_dir(&temporary).unwrap())
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(within, ["bases"]);
    assert_eq!(mode(&temporary), 0o700);
    assert_eq!(mode(&temporary.join("bases")), 0o700);
}
