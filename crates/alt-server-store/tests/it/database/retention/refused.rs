//! A request the database does not let go of.
use super::*;

/// Seven requests are over long enough, and the database refuses to remove one
/// in the middle of them. The round that comes to it removes the others it looks
/// at, tells of the one, and passes it; the rounds behind it remove the rest. The
/// one is come to again in the next pass, and goes once nothing holds it.
#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn a_request_the_database_does_not_let_go_of_is_told_of_and_keeps_none_behind_it() {
    let mut f = Fixture::new();
    Payload::new("a", None).publish_new(&mut f.store, &f.credential);
    f.client()
        .batch_execute(&format!(
            "INSERT INTO alt_requests SELECT (jsonb_populate_record(q, jsonb_build_object('request_id','n'||n))).* FROM alt_requests q, generate_series(1,6) n;
             UPDATE alt_requests SET settled_at=settled_at-{};
             CREATE FUNCTION held_request() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF OLD.request_id='n2' THEN RAISE EXCEPTION 'a row that names this request'; END IF; RETURN OLD; END $$;
             CREATE TRIGGER held_request BEFORE DELETE ON alt_requests FOR EACH ROW EXECUTE FUNCTION held_request()",
            31 * DAY
        ))
        .unwrap();
    let mut seen = RequestsSeen::default();
    let mut failed = Vec::new();
    let mut round = |f: &mut Fixture, failed: &mut Vec<String>| {
        let mut tell = |rule: &str, _: alt_server_store::Error| failed.push(rule.to_owned());
        let round = f.store.remove_what_is_over(3, &mut seen, &mut tell);
        (round.requests, round.more)
    };
    // a, n1 and n2 are looked at: two go, and n2 is told of
    assert_eq!(round(&mut f, &mut failed), (2, true));
    assert_eq!(failed, ["requests"]);
    assert_eq!(requests(&f), ["n2", "n3", "n4", "n5", "n6"]);
    assert_eq!(round(&mut f, &mut failed), (3, true));
    assert_eq!(round(&mut f, &mut failed), (1, false));
    assert_eq!(failed, ["requests"]);
    assert_eq!(requests(&f), ["n2"]);
    // the next pass comes to it again
    assert_eq!(round(&mut f, &mut failed), (0, false));
    assert_eq!(failed, ["requests", "requests"]);
    f.client()
        .batch_execute("DROP TRIGGER held_request ON alt_requests")
        .unwrap();
    assert_eq!(round(&mut f, &mut failed), (1, false));
    assert_eq!(requests(&f), Vec::<String>::new());
}
