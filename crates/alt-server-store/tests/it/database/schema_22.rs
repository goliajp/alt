//! Schema 22 records when a request ended and when a credential was revoked. These
//! hold the upgrade of an instance that has neither.
use super::*;

/// makes the database the schema 21 one it stands in for; harmless on one that is
pub(super) fn as_schema_21(f: &Fixture) {
    f.client()
        .batch_execute(
            "DELETE FROM alt_schema WHERE version=22;
             ALTER TABLE alt_requests DROP CONSTRAINT IF EXISTS alt_requests_settled_at;
             DROP INDEX IF EXISTS alt_requests_settled;
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

/// A committed request is given the moment in its receipt, a request that ended
/// otherwise and a revoked credential the moment of the upgrade, and what has not
/// ended none. The upgraded instance goes on ending requests with their moment.
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
    let rows = requests(&f);
    let states: Vec<&str> = rows.iter().map(|row| row.1.as_str()).collect();
    assert_eq!(states, ["committed", "expired", "receiving"]);
    assert!(rows[0].2.is_some());
    assert_eq!(rows[0].2, rows[0].3);
    assert!((before..=after).contains(&rows[1].2.unwrap()));
    assert_eq!(rows[2].2, None);
    let credentials = "SELECT revoked,revoked_at FROM alt_credentials ORDER BY revoked";
    let credentials: Vec<(bool, Option<i64>)> = (f.client().query(credentials, &[]).unwrap())
        .iter()
        .map(|row| (row.get(0), row.get(1)))
        .collect();
    assert_eq!(credentials.len(), 2);
    assert_eq!(credentials[0], (false, None));
    assert!(credentials[1].0 && (before..=after).contains(&credentials[1].1.unwrap()));
    let indexes = "SELECT indexname::text FROM pg_indexes WHERE indexname IN ('alt_requests_settled','alt_requests_open','alt_segments_request','alt_segments_owner') ORDER BY 1";
    let indexes: Vec<String> = (f.client().query(indexes, &[]).unwrap().iter())
        .map(|row| row.get(0))
        .collect();
    assert_eq!(
        indexes,
        [
            "alt_requests_open",
            "alt_requests_settled",
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
    assert!(matches!(
        super::schema_20::migrate(&mut f, 21),
        Err(alt_server_store::Error::Precondition(_))
    ));
}
