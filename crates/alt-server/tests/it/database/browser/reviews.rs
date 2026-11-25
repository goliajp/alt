//! How many reviews are verified for their pages at once.
use super::*;
use crate::database::sign_in_deadline::start;

/// Two workers: one review is verified for its page at a time. While the
/// page of one is held where it reads the repository, a second is told at
/// once to come again in a minute, and anything else is still answered by
/// the other worker. Once the first is let go, a page is taken again.
#[test]
#[ignore = "requires isolated PostgreSQL and loopback HTTP; exercised by browser acceptance and closing gate"]
fn a_review_beyond_those_verified_at_once_is_told_to_come_again() {
    let mut f = Fixture::new();
    let server = start(&mut f, |factory, options| {
        options.workers = 2;
        options.work_timeout = Duration::from_secs(60);
        factory.statement_timeout = Duration::from_secs(60);
    });
    let (store, admin) = (&mut f.store, &f.credential);
    store
        .set_grants(admin, "repository", "reader", true, false, &[])
        .unwrap();
    let (cookie, _) = login(&server, "reader");
    let page = "/web/repositories/repository/epochs/1/requests/owner/none/review";
    let asked = || {
        let asked = ureq::get(&format!("{}{page}", server.url))
            .timeout(Duration::from_secs(90))
            .set("Cookie", &cookie);
        match asked.call() {
            Ok(response) | Err(ureq::Error::Status(_, response)) => {
                let again = response.header("Retry-After").map(str::to_owned);
                (response.status(), again)
            }
            Err(error) => panic!("{error}"),
        }
    };
    let unheld = asked().0;
    assert_ne!(unheld, 503);

    // the segments of the repository are what a page reads first
    let mut holding = f.client();
    holding
        .batch_execute("BEGIN; LOCK TABLE alt_pack_segments IN ACCESS EXCLUSIVE MODE")
        .unwrap();
    std::thread::scope(|scope| {
        let first = scope.spawn(asked);
        let waits = "SELECT count(*) FROM pg_locks WHERE NOT granted AND relation='alt_pack_segments'::regclass";
        let until = std::time::Instant::now() + Duration::from_secs(60);
        while f.client().query_one(waits, &[]).unwrap().get::<_, i64>(0) == 0 {
            assert!(std::time::Instant::now() < until, "no page is held");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(asked(), (503, Some("60".to_owned())));
        assert_eq!(server.command("GET", "/v1/capabilities", &f.token).0, 200);
        holding.batch_execute("ROLLBACK").unwrap();
        assert_eq!(first.join().unwrap().0, unheld);
    });
    assert_eq!(asked().0, unheld);
}
