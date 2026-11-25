//! The row every transaction of a Cloud instance that writes takes first.

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn the_row_of_a_cloud_instance_is_written_for_its_clock_and_for_its_authority_only_when_that_went_on()
 {
    let f = super::cloud::cloud();
    let mut client = f.client();
    let written =
        "SELECT coalesce(n_tup_upd,0) FROM pg_stat_user_tables WHERE relname='alt_cloud_binding'";
    let backends = "SELECT count(*) FROM pg_stat_activity WHERE datname=current_database()";
    let recorded = "SELECT clock_high_water,authority_revision FROM alt_cloud_binding";
    let mut store = f.reopen();
    let open: i64 = client.query_one(backends, &[]).unwrap().get(0);
    let before: i64 = client.query_one(written, &[]).unwrap().get(0);
    let was = client.query_one(recorded, &[]).unwrap();
    let asked = 40;
    for _ in 0..asked {
        store.principal(&f.credential).unwrap();
    }
    // what a connection wrote is told when it ends
    drop(store);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while client.query_one(backends, &[]).unwrap().get::<_, i64>(0) != open - 1 {
        assert!(std::time::Instant::now() < deadline, "the connection stays");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let after: i64 = client.query_one(written, &[]).unwrap().get(0);
    let is = client.query_one(recorded, &[]).unwrap();
    assert!(
        is.get::<_, i64>(0) > was.get::<_, i64>(0),
        "the clock is recorded"
    );
    assert_eq!(is.get::<_, i64>(1), was.get::<_, i64>(1));
    // once a transaction for its clock at most; the opening of the store is two more
    assert!(
        after - before <= asked + 2,
        "{} writes for {asked} transactions",
        after - before
    );
}
