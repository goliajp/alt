use crate::{code, connect};
use alt_pg::STATEMENT_LIMIT;

fn prepared(client: &mut alt_pg::Client, statement: &str) -> Vec<String> {
    client
        .query(
            "SELECT name FROM pg_prepared_statements WHERE statement=$1",
            &[&statement],
        )
        .unwrap()
        .iter()
        .map(|row| row.get(0))
        .collect()
}

fn held(client: &mut alt_pg::Client) -> i64 {
    client
        .query_one("SELECT count(*) FROM pg_prepared_statements", &[])
        .unwrap()
        .get(0)
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn a_statement_is_prepared_once_per_connection() {
    let mut client = connect();
    let sql = "SELECT $1::int8+1";
    for value in 0..5i64 {
        assert_eq!(
            client.query_one(sql, &[&value]).unwrap().get::<_, i64>(0),
            value + 1
        );
    }
    let mut tx = client.transaction().unwrap();
    assert_eq!(tx.query_one(sql, &[&9i64]).unwrap().get::<_, i64>(0), 10);
    tx.commit().unwrap();
    assert_eq!(prepared(&mut client, sql).len(), 1);
    assert_eq!(client.cached_statements(), 2);
    // a statement that failed to prepare is not kept
    assert_eq!(
        code(client.query("SELECT nothing FROM nowhere", &[])),
        "42P01"
    );
    assert_eq!(code(client.query("SELEC", &[])), "42601");
    assert_eq!(client.cached_statements(), 2);
    let mut other = connect();
    assert_eq!(other.cached_statements(), 0);
    assert!(prepared(&mut other, sql).is_empty());
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn the_cache_is_bounded_on_both_ends() {
    let mut client = connect();
    for value in 0..STATEMENT_LIMIT as i64 + 40 {
        let sql = format!("SELECT {value}::int8");
        assert_eq!(client.query_one(&sql, &[]).unwrap().get::<_, i64>(0), value);
    }
    assert_eq!(client.cached_statements(), STATEMENT_LIMIT);
    // a displaced statement is closed on the server with the request that follows
    assert_eq!(held(&mut client), STATEMENT_LIMIT as i64);
    assert_eq!(held(&mut client), STATEMENT_LIMIT as i64);
    // a displaced statement is prepared again when it returns
    assert!(prepared(&mut client, "SELECT 0::int8").is_empty());
    assert_eq!(
        client
            .query_one("SELECT 0::int8", &[])
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    assert_eq!(prepared(&mut client, "SELECT 0::int8").len(), 1);
    client.clear_statements();
    assert_eq!(client.cached_statements(), 0);
    // closes also ride ahead of a batch
    client.batch_execute("SELECT 1").unwrap();
    assert_eq!(held(&mut client), 1);
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn clearing_lets_a_kept_statement_change_its_result_type() {
    let mut client = connect();
    client
        .batch_execute("CREATE TEMP TABLE retyped(value int8); INSERT INTO retyped VALUES(3)")
        .unwrap();
    let sql = "SELECT value FROM retyped";
    assert_eq!(client.query_one(sql, &[]).unwrap().get::<_, i64>(0), 3);
    client
        .batch_execute("ALTER TABLE retyped ALTER COLUMN value TYPE text")
        .unwrap();
    assert_eq!(code(client.query_one(sql, &[])), "0A000");
    assert!(!client.is_closed());
    client.clear_statements();
    assert_eq!(client.query_one(sql, &[]).unwrap().get::<_, &str>(0), "3");
}

/// how often the kept statement was planned once for all executions, and for the values of one
fn plans(client: &mut alt_pg::Client, statement: &str) -> (i64, i64) {
    let row = client
        .query_one(
            "SELECT generic_plans,custom_plans FROM pg_prepared_statements WHERE statement=$1",
            &[&statement],
        )
        .unwrap();
    (row.get(0), row.get(1))
}

fn mode(client: &mut alt_pg::Client) -> String {
    client
        .query_one("SELECT current_setting('plan_cache_mode')", &[])
        .unwrap()
        .get(0)
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn a_statement_is_planned_for_its_values_where_asked_and_nothing_else_is() {
    let mut client = connect();
    client
        .batch_execute("SET plan_cache_mode = force_generic_plan")
        .unwrap();
    let sql = "SELECT count(*) FROM unnest($1::int8[]) v WHERE v=ANY($1)";
    let values = vec![1i64, 2, 2];
    for _ in 0..3 {
        let rows = client.query_for_values(sql, &[&values]).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get::<_, i64>(0), 3);
        assert_eq!(mode(&mut client), "force_generic_plan");
    }
    assert_eq!(plans(&mut client, sql), (0, 3));
    assert_eq!(
        client.query_one(sql, &[&values]).unwrap().get::<_, i64>(0),
        3
    );
    assert_eq!(plans(&mut client, sql), (1, 3));

    client
        .batch_execute("CREATE TEMP TABLE planned(v int8); INSERT INTO planned VALUES(1),(2),(3)")
        .unwrap();
    let delete = "DELETE FROM planned WHERE v=ANY($1)";
    let other = "SELECT count(*) FROM planned WHERE v<>$1";
    let mut tx = client.transaction().unwrap();
    // the first statement of a transaction opens it in the same request
    assert_eq!(tx.execute_for_values(delete, &[&values]).unwrap(), 2);
    assert_eq!(tx.query_one(other, &[&0i64]).unwrap().get::<_, i64>(0), 1);
    assert_eq!(tx.execute_for_values(delete, &[&vec![3i64]]).unwrap(), 1);
    let left = tx.query_for_values(other, &[&9i64]).unwrap();
    assert_eq!(left[0].get::<_, i64>(0), 0);
    assert_eq!(tx.query_one(other, &[&0i64]).unwrap().get::<_, i64>(0), 0);
    tx.commit().unwrap();
    assert_eq!(plans(&mut client, delete), (0, 2));
    assert_eq!(plans(&mut client, other), (2, 1));
    assert_eq!(mode(&mut client), "force_generic_plan");

    // a failure leaves the session as it was, inside a transaction and outside
    assert_eq!(
        code(client.query_for_values("SELECT 1/$1::int8", &[&0i64])),
        "22012"
    );
    assert_eq!(mode(&mut client), "force_generic_plan");
    let mut tx = client.transaction().unwrap();
    assert_eq!(
        code(tx.execute_for_values("SELECT 1/$1::int8", &[&0i64])),
        "22012"
    );
    tx.rollback().unwrap();
    assert_eq!(mode(&mut client), "force_generic_plan");
    assert_eq!(
        client.query_one(other, &[&0i64]).unwrap().get::<_, i64>(0),
        0
    );

    // statements prepared for one use are bracketed the same way
    let mut tx = client.transaction().unwrap().uncached();
    assert_eq!(tx.execute_for_values(delete, &[&values]).unwrap(), 0);
    assert_eq!(
        tx.query_one("SELECT current_setting('plan_cache_mode')", &[])
            .unwrap()
            .get::<_, String>(0),
        "force_generic_plan"
    );
    tx.commit().unwrap();
}
