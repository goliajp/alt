//! Schema 22 records when a request ended and when a credential was revoked. These
//! hold the upgrade of an instance that has neither.
use super::*;

mod earlier_requests;

/// makes the database the schema 21 one it stands in for; harmless on one that is
pub(super) fn as_schema_21(f: &Fixture) {
    f.client()
        .batch_execute(
            "DELETE FROM alt_schema WHERE version=22;
             ALTER TABLE alt_requests DROP CONSTRAINT IF EXISTS alt_requests_settled_at;
             DROP INDEX IF EXISTS alt_requests_settled;
             DROP INDEX IF EXISTS alt_requests_unsettled;
             DROP INDEX IF EXISTS alt_requests_open;
             ALTER TABLE alt_requests DROP COLUMN IF EXISTS settled_at;
             ALTER TABLE alt_credentials DROP CONSTRAINT IF EXISTS alt_credentials_revoked_at;
             ALTER TABLE alt_credentials DROP COLUMN IF EXISTS revoked_at;
             DROP INDEX IF EXISTS alt_segments_request;
             CREATE INDEX IF NOT EXISTS alt_segments_owner ON alt_segments(repository,epoch,actor_id,request_id) WHERE state='uploading'",
        )
        .unwrap();
}

/// state, when it ended and when its receipt says it was committed, by request
fn requests(f: &Fixture) -> Vec<(String, String, Option<i64>, Option<i64>)> {
    let rows = "SELECT request_id,state,settled_at,(result#>>'{receipt,body,committed_at_unix_ms}')::bigint FROM alt_requests ORDER BY request_id";
    (f.client().query(rows, &[]).unwrap().iter())
        .map(|row| (row.get(0), row.get(1), row.get(2), row.get(3)))
        .collect()
}
fn now(f: &Fixture) -> i64 {
    let time = "SELECT floor(extract(epoch FROM clock_timestamp())*1000)::bigint";
    f.client().query_one(time, &[]).unwrap().get(0)
}

/// The upgrade gives a revoked credential the moment of the upgrade and writes no
/// request: the ones that are over are counted as waiting for their moment. A
/// round of the server gives a committed request the moment in its receipt and one
/// that ended otherwise the moment of the round, and what has not ended none. The
/// upgraded instance goes on ending requests with their moment.
#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn an_older_instance_is_given_the_moments_its_requests_and_credentials_ended() {
    let mut f = Fixture::new();
    let first = Payload::new("a-committed", None);
    first.publish_new(&mut f.store, &f.credential);
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
        .batch_execute("INSERT INTO alt_credentials(token_digest,actor_id,attribution,identity_source,expires_at,revoked,revoked_at) SELECT repeat('a',64),actor_id,attribution,identity_source,expires_at,true,1 FROM alt_credentials")
        .unwrap();

    as_schema_21(&f);
    assert!(f.store.check_connection().is_err());
    let before = now(&f);
    super::schema_20::migrate(&mut f, 21).unwrap();
    let after = now(&f);
    assert!(requests(&f).iter().all(|row| row.2.is_none()));
    assert_eq!(f.store.requests_without_moment().unwrap(), 2);
    let given = f.store.give_earlier_requests_their_moment(100, 1).unwrap();
    assert_eq!(given, (2, false));
    assert_eq!(f.store.requests_without_moment().unwrap(), 0);
    let rows = requests(&f);
    let states: Vec<&str> = rows.iter().map(|row| row.1.as_str()).collect();
    assert_eq!(states, ["committed", "expired", "receiving"]);
    assert!(rows[0].2.is_some());
    assert_eq!(rows[0].2, rows[0].3);
    assert!((after..=now(&f)).contains(&rows[1].2.unwrap()));
    assert_eq!(rows[2].2, None);
    let credentials = "SELECT revoked,revoked_at FROM alt_credentials ORDER BY revoked";
    let credentials: Vec<(bool, Option<i64>)> = (f.client().query(credentials, &[]).unwrap())
        .iter()
        .map(|row| (row.get(0), row.get(1)))
        .collect();
    assert_eq!(credentials.len(), 2);
    assert_eq!(credentials[0], (false, None));
    assert!(credentials[1].0 && (before..=after).contains(&credentials[1].1.unwrap()));
    let indexes = "SELECT indexname::text FROM pg_indexes WHERE indexname IN ('alt_requests_settled','alt_requests_unsettled','alt_requests_open','alt_segments_request','alt_segments_owner') ORDER BY 1";
    let indexes: Vec<String> = (f.client().query(indexes, &[]).unwrap().iter())
        .map(|row| row.get(0))
        .collect();
    assert_eq!(
        indexes,
        [
            "alt_requests_open",
            "alt_requests_settled",
            "alt_requests_unsettled",
            "alt_segments_request"
        ]
    );

    // the request left open ends with its moment, and publishing goes on
    f.client()
        .batch_execute("UPDATE alt_requests SET expires_at=0 WHERE request_id='c-open'")
        .unwrap();
    assert_eq!(f.store.expire_requests(100, &mut |_, _| ()).unwrap(), 1);
    Payload::new("d-next", Some(first.commit)).publish_new(&mut f.store, &f.credential);
    assert!(requests(&f).iter().all(|row| row.2.is_some()));
    assert_eq!(f.store.requests_without_moment().unwrap(), 0);
    assert!(matches!(
        super::schema_20::migrate(&mut f, 21),
        Err(alt_server_store::Error::Precondition(_))
    ));
}

#[test]
#[ignore = "a measurement of the upgrade to schema 22 and of the rounds after it, for --nocapture and with ALT_MEASURE_REQUESTS; requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn the_time_and_the_size_of_the_upgrade_by_how_many_requests_there_are() {
    let told = std::env::var("ALT_MEASURE_REQUESTS").ok();
    let copies: i64 = told.and_then(|told| told.parse().ok()).unwrap_or(1000);
    let mut f = Fixture::new();
    Payload::new("committed", None).publish_new(&mut f.store, &f.credential);
    // as many requests as that, each as the one that was committed left its row
    let copy = format!(
        "INSERT INTO alt_requests SELECT (jsonb_populate_record(q, jsonb_build_object('request_id', 'copy-' || n))).* FROM alt_requests q, generate_series(1, {copies}) n"
    );
    f.client().batch_execute(&copy).unwrap();
    as_schema_21(&f);
    f.client().batch_execute("VACUUM alt_requests").unwrap();
    let size = |f: &Fixture| -> (i64, i64) {
        let size = "SELECT pg_table_size('alt_requests'),pg_indexes_size('alt_requests')";
        let row = f.client().query_one(size, &[]).unwrap();
        (row.get(0), row.get(1))
    };
    let before = size(&f);
    let began = std::time::Instant::now();
    super::schema_20::migrate(&mut f, 21).unwrap();
    println!(
        "{} requests, table and indexes {before:?} bytes: upgraded in {:?}, {:?} bytes after",
        copies + 1,
        began.elapsed(),
        size(&f)
    );
    let (began, mut rounds, mut largest) = (std::time::Instant::now(), 0, size(&f));
    loop {
        let (_, more) = (f.store)
            .give_earlier_requests_their_moment(1000, 20)
            .unwrap();
        rounds += 1;
        let now = size(&f);
        largest = (largest.0.max(now.0), largest.1.max(now.1));
        if !more {
            break;
        }
    }
    println!(
        "moments given in {rounds} rounds and {:?}: {:?} bytes after, {largest:?} at the most",
        began.elapsed(),
        size(&f)
    );
    let settled = "SELECT count(*) FROM alt_requests WHERE settled_at IS NOT NULL";
    let settled: i64 = f.client().query_one(settled, &[]).unwrap().get(0);
    assert_eq!(settled, copies + 1);
}
