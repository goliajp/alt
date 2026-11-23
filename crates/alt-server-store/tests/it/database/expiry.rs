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
    assert_eq!(f.store.expire_requests(100, &mut |_, _| ()).unwrap(), 0);
    f.client()
        .execute(
            "UPDATE alt_requests SET expires_at=0 WHERE request_id='a-late'",
            &[],
        )
        .unwrap();
    assert_eq!(f.store.expire_requests(100, &mut |_, _| ()).unwrap(), 1);
    let states = vec!["expired".to_owned(), "receiving".to_owned()];
    assert_eq!(held(&f), (in_time, states));
    assert_eq!(f.store.expire_requests(100, &mut |_, _| ()).unwrap(), 0);
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
    assert_eq!(restarted.expire_requests(100, &mut |_, _| ()).unwrap(), 1);
    assert_eq!(held(&f), (0, vec!["expired".to_owned()]));
}

/// Two repositories have a request to end, and the first cannot be gone into:
/// its failure is told, and the second has its request ended in the same round.
/// A request of an epoch before a recovery is left as it is and keeps no round
/// coming back for it.
#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn a_repository_that_cannot_be_gone_into_keeps_no_other_from_its_round() {
    let mut f = Fixture::new();
    begun(&mut f, "late");
    let mut client = f.client();
    // a second repository, before the first by its name, with the same request
    client
        .batch_execute(
            "INSERT INTO alt_repositories SELECT (jsonb_populate_record(r, '{\"id\":\"a-first\"}')).* FROM alt_repositories r WHERE id='repository';
             INSERT INTO alt_requests SELECT (jsonb_populate_record(q, '{\"repository\":\"a-first\"}')).* FROM alt_requests q;
             UPDATE alt_requests SET expires_at=0;
             UPDATE alt_repositories SET reserved_bytes=0 WHERE id='a-first'",
        )
        .unwrap();
    let mut failed = Vec::new();
    let ended = f
        .store
        .expire_requests(100, &mut |repository, _| failed.push(repository.to_owned()));
    assert_eq!((ended.unwrap(), failed), (1, vec!["a-first".to_owned()]));
    let states = "SELECT repository,state FROM alt_requests ORDER BY repository";
    let states: Vec<(String, String)> = (client.query(states, &[]).unwrap().iter())
        .map(|row| (row.get(0), row.get(1)))
        .collect();
    let (first, second) = (("a-first", "receiving"), ("repository", "expired"));
    let expected = [first, second].map(|(name, state)| (name.to_owned(), state.to_owned()));
    assert_eq!(states, expected);

    // the one that is left belongs to an epoch that is over
    client
        .batch_execute(
            "UPDATE alt_repositories SET recovery_epoch=recovery_epoch+1 WHERE id='a-first'",
        )
        .unwrap();
    let mut failed = Vec::new();
    let ended = f
        .store
        .expire_requests(100, &mut |repository, _| failed.push(repository.to_owned()));
    assert_eq!((ended.unwrap(), failed), (0, Vec::new()));
}
