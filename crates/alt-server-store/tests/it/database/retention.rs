//! What is over is removed after its time, and nothing sooner: requests,
//! what stood for a sign-in, and the credentials under it.
use super::*;
use alt_server_store::{Removed, RequestsSeen};

const DAY: i64 = 24 * 60 * 60 * 1000;

fn round(f: &mut Fixture, seen: &mut RequestsSeen) -> Removed {
    let mut failed = Vec::new();
    let removed = (f.store).remove_what_is_over(100, seen, &mut |rule, error| {
        failed.push(format!("{rule}: {error:?}"))
    });
    assert_eq!(failed, Vec::<String>::new());
    removed
}
fn texts(f: &Fixture, sql: &str) -> Vec<String> {
    (f.client().query(sql, &[]).unwrap().iter())
        .map(|row| row.get(0))
        .collect()
}
fn requests(f: &Fixture) -> Vec<String> {
    texts(f, "SELECT request_id FROM alt_requests ORDER BY request_id")
}
fn begun(f: &mut Fixture, request: &str) {
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

/// Requests that were committed and expired are there for thirty days and gone
/// after them, with a round that removes nothing before. One whose publication
/// carried records stays, and is not looked at by the round after. The event of
/// the removed publication is still told whole. A request an epoch before a
/// recovery left open goes thirty days after its time to upload, with the
/// contents it had named.
#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn a_request_that_is_over_is_kept_thirty_days_and_one_with_records_for_good() {
    let mut f = Fixture::new();
    let first = Payload::new("a-committed", None);
    let receipt = first.publish_new(&mut f.store, &f.credential);
    Payload::new("b-records", Some(first.commit)).publish_new(&mut f.store, &f.credential);
    begun(&mut f, "c-expired");
    f.client()
        .batch_execute("UPDATE alt_requests SET expires_at=0 WHERE request_id='c-expired'")
        .unwrap();
    assert_eq!(f.store.expire_requests(100, &mut |_, _| ()).unwrap(), 1);
    begun(&mut f, "d-open");
    let event = (f.store)
        .outbox_event("repository", receipt.body.transaction_sequence)
        .unwrap()
        .unwrap();
    // what makes the second one the history of records
    f.client()
        .batch_execute("INSERT INTO alt_task_bundles(repository,bundle_blake3,epoch,actor_id,request_id) SELECT repository,(SELECT blake3 FROM alt_blobs LIMIT 1),epoch,actor_id,request_id FROM alt_requests WHERE request_id='b-records'")
        .unwrap();

    let mut seen = RequestsSeen::default();
    assert_eq!(round(&mut f, &mut seen), Removed::default());
    let all = ["a-committed", "b-records", "c-expired", "d-open"];
    assert_eq!(requests(&f), all);

    f.client()
        .batch_execute(&format!(
            "UPDATE alt_requests SET settled_at=settled_at-{} WHERE settled_at IS NOT NULL",
            30 * DAY - 60_000
        ))
        .unwrap();
    assert_eq!(round(&mut f, &mut seen), Removed::default());
    f.client()
        .batch_execute(
            "UPDATE alt_requests SET settled_at=settled_at-120000 WHERE settled_at IS NOT NULL",
        )
        .unwrap();
    let removed = round(&mut f, &mut seen);
    let expected = Removed {
        requests: 2,
        ..Removed::default()
    };
    assert_eq!(removed, expected);
    assert_eq!(requests(&f), ["b-records", "d-open"]);
    let after = (f.store)
        .outbox_event("repository", receipt.body.transaction_sequence)
        .unwrap()
        .unwrap();
    assert_eq!(after, event);

    // every request that is over was looked at, so the next round begins again:
    // the one that stayed goes once its records are gone
    f.client()
        .batch_execute("DELETE FROM alt_task_bundles")
        .unwrap();
    assert_eq!(round(&mut f, &mut seen), expected_one());
    assert_eq!(requests(&f), ["d-open"]);

    // a recovery leaves the open request to an epoch that is over
    let contents = "SELECT count(*)::text FROM alt_request_content";
    assert_ne!(texts(&f, contents), ["0"]);
    f.client()
        .batch_execute("UPDATE alt_repositories SET recovery_epoch=recovery_epoch+1")
        .unwrap();
    assert_eq!(round(&mut f, &mut seen), Removed::default());
    f.client()
        .batch_execute(&format!(
            "UPDATE alt_requests SET expires_at=floor(extract(epoch FROM clock_timestamp())*1000)::bigint-{}",
            30 * DAY + 60_000
        ))
        .unwrap();
    let removed = round(&mut f, &mut seen);
    let expected = Removed {
        unfinished_requests: 1,
        ..Removed::default()
    };
    assert_eq!(removed, expected);
    assert_eq!(requests(&f), Vec::<String>::new());
    assert_eq!(texts(&f, contents), ["0"]);
}
fn expected_one() -> Removed {
    Removed {
        requests: 1,
        ..Removed::default()
    }
}

/// A round takes no more than it may and says that there is more.
#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn a_round_removes_so_many_and_tells_of_the_rest() {
    let mut f = Fixture::new();
    Payload::new("a", None).publish_new(&mut f.store, &f.credential);
    f.client()
        .batch_execute(&format!(
            "INSERT INTO alt_requests SELECT (jsonb_populate_record(q, jsonb_build_object('request_id','n'||n))).* FROM alt_requests q, generate_series(1,6) n;
             UPDATE alt_requests SET settled_at=settled_at-{}",
            31 * DAY
        ))
        .unwrap();
    let mut seen = RequestsSeen::default();
    let mut none = |_: &str, _| panic!("a rule failed");
    let first = f.store.remove_what_is_over(3, &mut seen, &mut none);
    assert_eq!((first.requests, first.more), (3, true));
    let second = f.store.remove_what_is_over(3, &mut seen, &mut none);
    assert_eq!((second.requests, second.more), (3, true));
    let third = f.store.remove_what_is_over(3, &mut seen, &mut none);
    assert_eq!((third.requests, third.more), (1, false));
    assert_eq!(requests(&f), Vec::<String>::new());
}

/// Seven requests stay for the records they carried. Three at a time they are
/// passed, each round going on behind the one before; with all of them looked at
/// the rounds begin again, and remove them once their records are gone.
#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn the_requests_that_stay_are_passed_and_looked_at_again_in_the_next_pass() {
    let mut f = Fixture::new();
    Payload::new("a", None).publish_new(&mut f.store, &f.credential);
    f.client()
        .batch_execute(&format!(
            "INSERT INTO alt_requests SELECT (jsonb_populate_record(q, jsonb_build_object('request_id','n'||n))).* FROM alt_requests q, generate_series(1,6) n;
             UPDATE alt_requests SET settled_at=settled_at-{};
             INSERT INTO alt_task_bundles(repository,bundle_blake3,epoch,actor_id,request_id) SELECT repository,(SELECT blake3 FROM alt_blobs LIMIT 1),epoch,actor_id,request_id FROM alt_requests",
            31 * DAY
        ))
        .unwrap();
    let mut seen = RequestsSeen::default();
    let mut none = |_: &str, _| panic!("a rule failed");
    let mut pass = |f: &mut Fixture| -> Vec<(u64, bool)> {
        (0..3)
            .map(|_| {
                let round = f.store.remove_what_is_over(3, &mut seen, &mut none);
                (round.requests, round.more)
            })
            .collect()
    };
    assert_eq!(pass(&mut f), [(0, true), (0, true), (0, false)]);
    assert_eq!(pass(&mut f), [(0, true), (0, true), (0, false)]);
    f.client()
        .batch_execute("DELETE FROM alt_task_bundles")
        .unwrap();
    assert_eq!(pass(&mut f), [(3, true), (3, true), (1, false)]);
}

/// Credentials by what became of them and what still names them. `e` is how long
/// ago one expired and `r` how long ago it was revoked, in days.
#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn a_credential_goes_ninety_days_after_it_ended_once_nothing_names_it() {
    let mut f = Fixture::new();
    let mut client = f.client();
    let credential = |name: &str, expired: i64, revoked: Option<i64>| {
        let revoked = match revoked {
            Some(days) => format!(
                "true,floor(extract(epoch FROM clock_timestamp())*1000)::bigint-{}",
                days * DAY
            ),
            None => "false,NULL".into(),
        };
        format!(
            "INSERT INTO alt_credentials(token_digest,actor_id,attribution,identity_source,expires_at,revoked,revoked_at) SELECT '{name}',actor_id,attribution,identity_source,floor(extract(epoch FROM clock_timestamp())*1000)::bigint-({expired}),{revoked} FROM alt_credentials WHERE actor_id='owner' AND length(token_digest)=64;"
        )
    };
    let now = "floor(extract(epoch FROM clock_timestamp())*1000)::bigint";
    let mut sql = String::new();
    sql += &credential("expired-91", 91 * DAY, None);
    sql += &credential("expired-89", 89 * DAY, None);
    sql += &credential("revoked-91-not-expired", -DAY, Some(91));
    sql += &credential("revoked-91-expired", DAY, Some(91));
    sql += &credential("revoked-89-expired", DAY, Some(89));
    sql += &credential("of-a-device", 91 * DAY, None);
    sql += &credential("parent", 91 * DAY, None);
    sql += &credential("delegate", -DAY, None);
    sql += &credential("delegate-over", 31 * DAY, None);
    sql += &credential("session", -DAY, None);
    sql += &credential("session-revoked", -DAY, Some(31));
    sql += &credential("approver", 91 * DAY, None);
    sql += "INSERT INTO alt_client_credentials(token_digest,client_id,created_at) VALUES('of-a-device','cli',1);";
    sql += "INSERT INTO alt_delegations(token_digest,parent_digest,scope,purpose,delegation_id,created_at) VALUES('delegate','parent','{}','p','one',1),('delegate-over','session','{}','p','two',1);";
    sql += "INSERT INTO alt_web_accounts(actor_id,login_name,display_name,password_hash,administrator) VALUES('owner','owner','Owner','-',true);";
    sql += &format!(
        "INSERT INTO alt_web_sessions(token_digest,actor_id,account_generation,expires_at,created_at) VALUES('session','owner',1,{now}-{},1),('session-revoked','owner',1,{now}+{DAY},1);",
        31 * DAY
    );
    sql += &format!(
        "INSERT INTO alt_device_authorizations(device_digest,user_digest,credential_digest,state,approved_by,created_at,expires_at,next_poll_at,interval_ms) VALUES('over','u1','c1','approved','approver',1,{now}-1,1,5000),('pending','u2','c2','pending',NULL,1,{now}+{DAY},1,5000);"
    );
    client.batch_execute(&sql).unwrap();

    let removed = round(&mut f, &mut RequestsSeen::default());
    let expected = Removed {
        web_sessions: 2,
        client_credentials: 1,
        delegations: 1,
        device_authorizations: 1,
        // expired-91, revoked-91-expired, of-a-device, approver
        credentials: 4,
        ..Removed::default()
    };
    assert_eq!(removed, expected);
    let left = texts(
        &f,
        "SELECT token_digest FROM alt_credentials WHERE length(token_digest)<64 ORDER BY 1",
    );
    let kept = [
        "delegate",
        "delegate-over",
        "expired-89",
        "parent",
        "revoked-89-expired",
        "revoked-91-not-expired",
        "session",
        "session-revoked",
    ];
    assert_eq!(left, kept);
    assert_eq!(
        texts(&f, "SELECT token_digest FROM alt_delegations"),
        ["delegate"]
    );
    assert_eq!(
        texts(&f, "SELECT device_digest FROM alt_device_authorizations"),
        ["pending"]
    );
    // the owner of the fixture is as it was
    f.store.authenticate(&f.token).unwrap();
    assert_eq!(
        round(&mut f, &mut RequestsSeen::default()),
        Removed::default()
    );
}
