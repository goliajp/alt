//! The requests that ended before schema 22 are given the moment they ended by
//! the server as it runs, so many at a time. These hold what they are given to
//! what one statement over all of them gave, and the removal to leaving them
//! until then.
use super::*;
use alt_server_store::{Removed, RequestsSeen};

const AT_ONCE: &str = "UPDATE alt_requests SET settled_at=CASE WHEN state='committed' THEN (result#>>'{receipt,body,committed_at_unix_ms}')::bigint ELSE floor(extract(epoch FROM clock_timestamp())*1000)::bigint END WHERE state IN ('committed','rejected','expired')";
const MOMENTS: &str = "SELECT request_id,state,settled_at FROM alt_requests ORDER BY request_id";

fn now(client: &mut Client) -> i64 {
    let time = "SELECT floor(extract(epoch FROM clock_timestamp())*1000)::bigint";
    client.query_one(time, &[]).unwrap().get(0)
}
fn moments(client: &mut Client) -> Vec<(String, String, Option<i64>)> {
    (client.query(MOMENTS, &[]).unwrap().iter())
        .map(|row| (row.get(0), row.get(1), row.get(2)))
        .collect()
}
/// one committed request and one expired, forty and fifteen more like them each
/// with a moment of its own long ago, and one still open; at schema 21
fn older_instance(f: &mut Fixture) {
    Payload::new("a-committed", None).publish_new(&mut f.store, &f.credential);
    for request in ["b-expired", "c-open"] {
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
        .batch_execute("UPDATE alt_requests SET expires_at=0 WHERE request_id='b-expired'")
        .unwrap();
    assert_eq!(f.store.expire_requests(100, &mut |_, _| ()).unwrap(), 1);
    f.client()
        .batch_execute(
            "INSERT INTO alt_requests SELECT (jsonb_populate_record(q, jsonb_build_object('request_id', 'd-committed-' || n, 'expires_at', n, 'result', jsonb_set(q.result, '{receipt,body,committed_at_unix_ms}', to_jsonb(1000 + n))))).* FROM alt_requests q, generate_series(1, 40) n WHERE q.request_id='a-committed';
             INSERT INTO alt_requests SELECT (jsonb_populate_record(q, jsonb_build_object('request_id', 'e-expired-' || n, 'expires_at', 100 + n))).* FROM alt_requests q, generate_series(1, 15) n WHERE q.request_id='b-expired'",
        )
        .unwrap();
    as_schema_21(f);
}

/// Fifty-seven requests are over. Seven at a time, in rounds of two transactions,
/// by one process and then by the one started after it, each is given what the
/// one statement of the upgrade used to give it: a committed request the moment in
/// its receipt to the millisecond, one that expired a moment no sooner than the
/// upgrade. The open one is given none. Until a request has its moment it is not
/// removed, however long ago it ended, and with it it is.
#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn requests_given_their_moment_a_few_at_a_time_have_the_one_they_were_given_at_once() {
    let mut f = Fixture::new();
    older_instance(&mut f);
    super::super::schema_20::migrate(&mut f, 21).unwrap();
    assert_eq!(f.store.requests_without_moment().unwrap(), 57);
    assert!(moments(&mut f.client()).iter().all(|row| row.2.is_none()));

    // what one statement gives, seen and taken back
    let mut one = f.client();
    let before = now(&mut one);
    one.batch_execute("BEGIN").unwrap();
    assert_eq!(one.execute(AT_ONCE, &[]).unwrap(), 57);
    let at_once = moments(&mut one);
    one.batch_execute("ROLLBACK").unwrap();
    let between = now(&mut one);

    // the committed ones ended long ago by their receipts, and stay
    let mut seen = RequestsSeen::default();
    let mut failed = Vec::new();
    let removed = (f.store).remove_what_is_over(100, &mut seen, &mut |rule, error| {
        failed.push(format!("{rule}: {error:?}"))
    });
    assert_eq!((removed, &failed), (Removed::default(), &vec![]));

    assert_eq!(
        f.store.give_earlier_requests_their_moment(7, 2).unwrap(),
        (14, true)
    );
    assert_eq!(f.store.requests_without_moment().unwrap(), 43);
    // another process goes on where this one was
    let mut next = f.reopen();
    let mut rounds = vec![];
    loop {
        let round = next.give_earlier_requests_their_moment(7, 2).unwrap();
        rounds.push(round.0);
        if !round.1 {
            break;
        }
    }
    assert_eq!(rounds, [14, 14, 14, 1]);
    assert_eq!(next.requests_without_moment().unwrap(), 0);
    assert_eq!(
        next.give_earlier_requests_their_moment(7, 2).unwrap(),
        (0, false)
    );

    let after = now(&mut one);
    let given = moments(&mut one);
    assert_eq!(given.len(), 58);
    for (given, at_once) in given.iter().zip(&at_once) {
        assert_eq!((&given.0, &given.1), (&at_once.0, &at_once.1));
        match given.1.as_str() {
            "committed" => assert_eq!(given.2, at_once.2, "{}", given.0),
            "expired" => {
                assert!((before..=between).contains(&at_once.2.unwrap()));
                assert!((between..=after).contains(&given.2.unwrap()), "{given:?}");
            }
            _ => assert_eq!((given.2, at_once.2), (None, None), "{}", given.0),
        }
    }
    let committed = given.iter().filter(|row| row.1 == "committed");
    let long_ago = committed.filter(|row| row.2.unwrap() < 2000).count();
    assert_eq!(long_ago, 40);

    let removed = (f.store).remove_what_is_over(100, &mut RequestsSeen::default(), &mut |_, _| ());
    assert_eq!(removed.requests, 40);
}

/// The upgrade waits for whoever has the instance for longer than a statement of
/// its connection is given, and is done: an index over every request takes the
/// time it takes.
#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn the_upgrade_is_not_held_to_the_time_a_statement_is_given() {
    let mut f = Fixture::new();
    older_instance(&mut f);
    let mut other = f.client();
    other
        .batch_execute("BEGIN; SELECT pg_advisory_xact_lock(715071909111)")
        .unwrap();
    let holding = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(1500));
        other.batch_execute("COMMIT").unwrap();
    });
    let mut client = f.client();
    client.batch_execute("SET statement_timeout=500").unwrap();
    let began = std::time::Instant::now();
    let migrated = Store::migrate(
        client,
        f.store.bytes().clone(),
        f.store.spec().clone(),
        SigningKey::from_bytes(&f.signing_seed),
        limits(),
        21,
    );
    holding.join().unwrap();
    let mut store = migrated.unwrap();
    assert!(began.elapsed() >= std::time::Duration::from_millis(1000));
    assert_eq!(store.requests_without_moment().unwrap(), 57);
}

/// Twenty thousand requests are given their moment two thousand to a round. The
/// table is vacuumed after each round, so the versions a round leaves behind are
/// written over by the next: when all have theirs the table is not a quarter
/// larger than it was, where one statement over all of them left it twice as
/// large.
#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn the_table_of_requests_grows_by_the_rows_of_a_round_and_no_more() {
    let mut f = Fixture::new();
    Payload::new("committed", None).publish_new(&mut f.store, &f.credential);
    f.client()
        .batch_execute("INSERT INTO alt_requests SELECT (jsonb_populate_record(q, jsonb_build_object('request_id', 'copy-' || n))).* FROM alt_requests q, generate_series(1, 20000) n")
        .unwrap();
    as_schema_21(&f);
    f.client().batch_execute("VACUUM alt_requests").unwrap();
    let size = |f: &Fixture| -> i64 {
        let size = "SELECT pg_table_size('alt_requests')";
        f.client().query_one(size, &[]).unwrap().get(0)
    };
    let before = size(&f);
    super::super::schema_20::migrate(&mut f, 21).unwrap();
    assert_eq!(size(&f), before);
    let (mut rounds, mut largest) = (0, before);
    loop {
        let (_, more) = (f.store)
            .give_earlier_requests_their_moment(500, 4)
            .unwrap();
        largest = largest.max(size(&f));
        if !more {
            break;
        }
        rounds += 1;
    }
    assert_eq!(rounds, 10);
    assert_eq!(f.store.requests_without_moment().unwrap(), 0);
    assert!(largest <= before + before / 4, "{before} to {largest}");
}

/// The vacuum of a round waits for another that has the table for longer than a
/// statement of the connection is given, and the round is done. The connection
/// holds its statements to that time afterwards as before.
#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn the_vacuum_of_a_round_is_not_held_to_the_time_a_statement_is_given() {
    let mut f = Fixture::new();
    older_instance(&mut f);
    super::super::schema_20::migrate(&mut f, 21).unwrap();
    let mut client = f.client();
    client.batch_execute("SET statement_timeout=500").unwrap();
    let mut store = Store::open(
        client,
        f.store.bytes().clone(),
        f.store.spec().clone(),
        SigningKey::from_bytes(&f.signing_seed),
        limits(),
    )
    .unwrap();
    let held = |lock: &'static str| {
        let mut other = f.client();
        other
            .batch_execute(&format!("BEGIN; LOCK TABLE alt_requests IN {lock} MODE"))
            .unwrap();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(1500));
            other.batch_execute("COMMIT").unwrap();
        })
    };
    // what a vacuum waits for, and a write of rows does not
    let holding = held("SHARE UPDATE EXCLUSIVE");
    let began = std::time::Instant::now();
    let given = store.give_earlier_requests_their_moment(100, 1);
    holding.join().unwrap();
    assert_eq!(given.unwrap(), (57, false));
    assert!(began.elapsed() >= std::time::Duration::from_millis(1000));
    // what every statement waits for
    let holding = held("ACCESS EXCLUSIVE");
    let asked = store.requests_without_moment();
    holding.join().unwrap();
    assert!(asked.is_err(), "{asked:?}");
}
