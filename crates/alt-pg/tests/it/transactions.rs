use crate::{code, connect};

fn count(client: &mut alt_pg::Client, table: &str) -> i64 {
    client
        .query_one(&format!("SELECT count(*) FROM {table}"), &[])
        .unwrap()
        .get(0)
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn commit_keeps_and_rollback_discards() {
    let mut client = connect();
    let mut observer = connect();
    let table = format!("alt_pg_tx_{}", std::process::id());
    client
        .batch_execute(&format!("CREATE TABLE {table}(id int8 PRIMARY KEY)"))
        .unwrap();
    let insert = format!("INSERT INTO {table} VALUES($1)");
    {
        let mut tx = client.transaction().unwrap();
        tx.execute(&insert, &[&1i64]).unwrap();
        tx.execute(&insert, &[&2i64]).unwrap();
        // uncommitted work is invisible to another connection
        assert_eq!(count(&mut observer, &table), 0);
        tx.commit().unwrap();
    }
    assert_eq!(count(&mut observer, &table), 2);
    {
        let mut tx = client.transaction().unwrap();
        tx.execute(&insert, &[&3i64]).unwrap();
        assert_eq!(
            tx.query(&format!("SELECT id FROM {table}"), &[])
                .unwrap()
                .len(),
            3
        );
    }
    assert_eq!(count(&mut observer, &table), 2);
    {
        let mut tx = client.transaction().unwrap();
        tx.execute(&insert, &[&4i64]).unwrap();
        tx.rollback().unwrap();
    }
    assert_eq!(count(&mut client, &table), 2);
    // a statement outside a block commits by itself
    client.execute(&insert, &[&5i64]).unwrap();
    assert_eq!(count(&mut observer, &table), 3);
    client
        .batch_execute(&format!("DROP TABLE {table}"))
        .unwrap();
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn one_block_spans_all_its_statements() {
    let mut client = connect();
    let id = "SELECT txid_current()::text,pg_backend_pid()";
    let (first, second) = {
        let mut tx = client.transaction().unwrap();
        let first: String = tx.query_one(id, &[]).unwrap().get(0);
        let again: String = tx.query_one(id, &[]).unwrap().get(0);
        assert_eq!(first, again);
        tx.commit().unwrap();
        let mut tx = client.transaction().unwrap();
        // a statement prepared for the first time inside the block joins it too
        let second: String = tx
            .query_one("SELECT txid_current()::text,1", &[])
            .unwrap()
            .get(0);
        assert_eq!(tx.query_one(id, &[]).unwrap().get::<_, String>(0), second);
        tx.commit().unwrap();
        (first, second)
    };
    assert_ne!(first, second);
    // a block opened by a batch covers the statements after it
    let mut tx = client.transaction().unwrap();
    tx.batch_execute("CREATE TEMP TABLE in_block(x int4); INSERT INTO in_block VALUES(1)")
        .unwrap();
    assert_eq!(
        tx.execute("INSERT INTO in_block VALUES($1)", &[&2i32])
            .unwrap(),
        1
    );
    drop(tx);
    assert_eq!(code(client.query("SELECT x FROM in_block", &[])), "42P01");
    // a block with no statements never reaches the server
    client.transaction().unwrap().commit().unwrap();
    client.transaction().unwrap().rollback().unwrap();
    drop(client.transaction().unwrap());
    let status: String = client
        .query_one(
            "SELECT coalesce(txid_current_if_assigned()::text,'none')",
            &[],
        )
        .unwrap()
        .get(0);
    assert_eq!(status, "none");
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn a_failed_block_stays_failed_until_it_ends() {
    let mut client = connect();
    client
        .batch_execute("CREATE TEMP TABLE kept(id int8 PRIMARY KEY)")
        .unwrap();
    {
        let mut tx = client.transaction().unwrap();
        tx.execute("INSERT INTO kept VALUES($1)", &[&1i64]).unwrap();
        assert_eq!(code(tx.query_one("SELECT 1/$1::int4", &[&0i32])), "22012");
        // the server refuses everything until the block is closed
        assert_eq!(code(tx.query_one("SELECT 1", &[])), "25P02");
        assert_eq!(code(tx.batch_execute("SELECT 1")), "25P02");
    }
    assert_eq!(count(&mut client, "kept"), 0);
    {
        // the first statement of a block failing leaves the block open and failed
        let mut tx = client.transaction().unwrap();
        assert_eq!(
            code(tx.execute("INSERT INTO missing VALUES(1)", &[])),
            "42P01"
        );
        assert_eq!(
            code(tx.execute("INSERT INTO kept VALUES($1)", &[&2i64])),
            "25P02"
        );
    }
    {
        let mut tx = client.transaction().unwrap();
        assert_eq!(
            code(tx.batch_execute("INSERT INTO kept VALUES(3); SELECT 1/0")),
            "22012"
        );
        assert_eq!(code(tx.query_one("SELECT 1", &[])), "25P02");
    }
    assert_eq!(count(&mut client, "kept"), 0);
    let mut tx = client.transaction().unwrap();
    tx.execute("INSERT INTO kept VALUES($1)", &[&4i64]).unwrap();
    tx.commit().unwrap();
    assert_eq!(count(&mut client, "kept"), 1);
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn an_uncached_block_follows_a_changing_table() {
    let mut client = connect();
    client
        .batch_execute("CREATE TEMP TABLE shifting(value int8); INSERT INTO shifting VALUES(7)")
        .unwrap();
    let sql = "SELECT value FROM shifting";
    let mut tx = client.transaction().unwrap().uncached();
    assert_eq!(tx.query_one(sql, &[]).unwrap().get::<_, i64>(0), 7);
    tx.batch_execute("ALTER TABLE shifting ALTER COLUMN value TYPE text")
        .unwrap();
    assert_eq!(tx.query_one(sql, &[]).unwrap().get::<_, &str>(0), "7");
    assert_eq!(
        tx.execute("UPDATE shifting SET value=$1", &[&"eight"])
            .unwrap(),
        1
    );
    tx.commit().unwrap();
    assert_eq!(client.cached_statements(), 0);
    assert_eq!(
        client.query_one(sql, &[]).unwrap().get::<_, &str>(0),
        "eight"
    );
    assert_eq!(client.cached_statements(), 1);
}
