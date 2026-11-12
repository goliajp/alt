use crate::connect;

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn a_large_result_arrives_complete() {
    let mut client = connect();
    let rows = client
        .query(
            "SELECT g::int8,repeat('x',(g%50)::int4),CASE WHEN g%7=0 THEN NULL ELSE g::int4 END \
             FROM generate_series(1,$1::int8) g",
            &[&200_000i64],
        )
        .unwrap();
    assert_eq!(rows.len(), 200_000);
    for (index, row) in rows.iter().enumerate() {
        let g = index as i64 + 1;
        assert_eq!(row.get::<_, i64>(0), g);
        assert_eq!(row.get::<_, &str>(1).len() as i64, g % 50);
        assert_eq!(
            row.get::<_, Option<i32>>(2),
            (g % 7 != 0).then_some(g as i32)
        );
    }
    // rows outlive the call that produced them and each other
    let kept = rows[199_999].clone();
    drop(rows);
    assert_eq!(client.query("SELECT 1", &[]).unwrap().len(), 1);
    assert_eq!(kept.get::<_, i64>(0), 200_000);
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn a_large_bytea_round_trips() {
    let mut client = connect();
    let mut data = vec![0u8; 48 << 20];
    for (index, byte) in data.iter_mut().enumerate() {
        *byte = (index * 31 + index / 251) as u8;
    }
    let row = client
        .query_one("SELECT $1::bytea,length($1::bytea)", &[&data])
        .unwrap();
    assert_eq!(row.get::<_, i32>(1) as usize, data.len());
    assert!(row.get::<_, &[u8]>(0) == data.as_slice());
    assert_eq!(
        client.query_one("SELECT 2", &[]).unwrap().get::<_, i32>(0),
        2
    );
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn row_counts_are_enforced_and_reported() {
    let mut client = connect();
    client
        .batch_execute("CREATE TEMP TABLE counted(id int8 PRIMARY KEY, word text)")
        .unwrap();
    assert_eq!(
        client
            .execute(
                "INSERT INTO counted SELECT g,'w' FROM generate_series(1,$1::int8) g",
                &[&5i64]
            )
            .unwrap(),
        5
    );
    assert_eq!(
        client
            .execute("UPDATE counted SET word='x' WHERE id>$1", &[&3i64])
            .unwrap(),
        2
    );
    assert_eq!(
        client
            .execute("DELETE FROM counted WHERE id=$1", &[&99i64])
            .unwrap(),
        0
    );
    // rows of an executed query are discarded, their count is not
    assert_eq!(client.execute("SELECT * FROM counted", &[]).unwrap(), 5);
    assert_eq!(
        client
            .execute("CREATE TEMP TABLE other(x int4)", &[])
            .unwrap(),
        0
    );
    let one = "SELECT id FROM counted WHERE id=$1";
    assert!(client.query_opt(one, &[&99i64]).unwrap().is_none());
    assert!(client.query_opt(one, &[&1i64]).unwrap().is_some());
    assert!(matches!(
        client.query_one(one, &[&99i64]),
        Err(alt_pg::Error::RowCount)
    ));
    let all = "SELECT id FROM counted";
    assert!(matches!(
        client.query_one(all, &[]),
        Err(alt_pg::Error::RowCount)
    ));
    assert!(matches!(
        client.query_opt(all, &[]),
        Err(alt_pg::Error::RowCount)
    ));
    assert_eq!(client.query(all, &[]).unwrap().len(), 5);
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn unusual_result_shapes_are_handled() {
    let mut client = connect();
    let rows = client
        .query("SELECT FROM generate_series(1,3)", &[])
        .unwrap();
    assert_eq!(rows.len(), 3);
    assert!(rows[0].is_empty());
    assert!(client.query("", &[]).unwrap().is_empty());
    client.batch_execute("").unwrap();
    client.batch_execute("SELECT 1; SELECT 2,3; ").unwrap();
    // a type without a decoder is still delivered, as its raw bytes are not checked
    let row = client
        .query_one("SELECT now(),'x'::text AS word", &[])
        .unwrap();
    assert_eq!(row.columns()[0].type_oid(), 1184);
    assert_eq!(row.columns()[1].name(), "word");
    assert!(row.try_get::<_, i64>(0).is_err());
    assert_eq!(row.get::<_, &str>("word"), "x");
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn messages_the_server_sends_unasked_are_skipped() {
    let mut client = connect();
    client
        .batch_execute("DO $$ BEGIN RAISE NOTICE 'from a batch'; END $$")
        .unwrap();
    client
        .execute("DO $$ BEGIN RAISE WARNING 'from a statement'; END $$", &[])
        .unwrap();
    // a changed setting is reported with a parameter status message
    client
        .execute("SET application_name='renamed'", &[])
        .unwrap();
    client
        .batch_execute("SET TimeZone='UTC'; LISTEN alt_pg_channel")
        .unwrap();
    client
        .execute("NOTIFY alt_pg_channel,'payload'", &[])
        .unwrap();
    let mut other = connect();
    other
        .execute("NOTIFY alt_pg_channel,'from elsewhere'", &[])
        .unwrap();
    for _ in 0..3 {
        assert_eq!(
            client
                .query_one("SELECT current_setting('application_name')", &[])
                .unwrap()
                .get::<_, &str>(0),
            "renamed"
        );
    }
}
