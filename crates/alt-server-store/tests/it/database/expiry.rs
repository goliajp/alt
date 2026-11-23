//! What a request reserved is free again when the request is over, however it
//! ended, and without an operator asking for it.
use super::*;

/// what the repository has reserved, and the states of its requests
fn held(f: &Fixture) -> (i64, Vec<String>) {
    let mut client = f.client();
    let reserved = "SELECT reserved_bytes FROM alt_repositories WHERE id='repository'";
    let states = "SELECT state FROM alt_requests ORDER BY request_id";
    let reserved: i64 = client.query_one(reserved, &[]).unwrap().get(0);
    let states = client.query(states, &[]).unwrap();
    (reserved, states.iter().map(|row| row.get(0)).collect())
}

fn begun(f: &mut Fixture, request: &str) -> Payload {
    let payload = Payload::new(request, None);
    f.store
        .begin(
            &f.credential,
            &payload.publication,
            &payload.manifest,
            60_000,
        )
        .unwrap();
    payload
}

/// The time to upload of one request is over and that of another is not: the
/// first is ended and its room is free, by a round nobody with any authority asked
/// for; the second keeps what it reserved.
#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn a_request_whose_time_to_upload_ran_out_is_ended_and_gives_back_what_it_reserved() {
    let mut f = Fixture::new();
    begun(&mut f, "a-late");
    let late = held(&f).0;
    begun(&mut f, "b-in-time");
    let in_time = held(&f).0 - late;
    assert!(late > 0 && in_time > 0);
    assert_eq!(f.store.expire_requests(100).unwrap(), 0);
    f.client()
        .execute(
            "UPDATE alt_requests SET expires_at=0 WHERE request_id='a-late'",
            &[],
        )
        .unwrap();
    assert_eq!(f.store.expire_requests(100).unwrap(), 1);
    let states = vec!["expired".to_owned(), "receiving".to_owned()];
    assert_eq!(held(&f), (in_time, states));
    assert_eq!(f.store.expire_requests(100).unwrap(), 0);
}

/// The process that took the request is gone, and its time ran out meanwhile: the
/// one started after it finds the request and ends it.
#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn a_request_left_by_a_process_that_is_gone_is_ended_by_the_next() {
    let mut f = Fixture::new();
    begun(&mut f, "left");
    assert!(held(&f).0 > 0);
    f.client()
        .execute("UPDATE alt_requests SET expires_at=0", &[])
        .unwrap();
    let mut restarted = f.reopen();
    assert_eq!(restarted.expire_requests(100).unwrap(), 1);
    assert_eq!(held(&f), (0, vec!["expired".to_owned()]));
}
