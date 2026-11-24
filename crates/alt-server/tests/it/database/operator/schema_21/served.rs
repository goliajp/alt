//! The requests an upgrade left without the moment they ended are given it by the
//! server as it runs, with nobody asking, and by the server started after it.
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
