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
