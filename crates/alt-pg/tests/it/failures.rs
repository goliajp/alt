use crate::{code, connect};
use alt_pg::Error;

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn server_errors_carry_their_fields_and_leave_the_connection_usable() {
    let mut client = connect();
    client
        .batch_execute("CREATE TEMP TABLE named(id int8 CONSTRAINT named_key PRIMARY KEY)")
        .unwrap();
    client
        .execute("INSERT INTO named VALUES($1)", &[&1i64])
        .unwrap();
    let error = client
        .execute("INSERT INTO named VALUES($1)", &[&1i64])
        .unwrap_err();
    let server = error.as_server().expect("a server error");
    assert_eq!(error.code(), Some("23505"));
    assert_eq!(server.severity, "ERROR");
    assert_eq!(server.constraint.as_deref(), Some("named_key"));
    assert_eq!(server.table.as_deref(), Some("named"));
    assert!(server.detail.as_deref().unwrap().contains("already exists"));
    assert!(error.to_string().contains("23505"));
    assert_eq!(code(client.query("SELEC 1", &[])), "42601");
    assert_eq!(
        code(client.query_one("SELECT 1/$1::int4", &[&0i32])),
        "22012"
    );
    assert_eq!(
        code(client.batch_execute("SELECT 1; SELECT 1/0; SELECT 2")),
        "22012"
    );
    client.batch_execute("SET statement_timeout=50").unwrap();
    assert_eq!(code(client.batch_execute("SELECT pg_sleep(5)")), "57014");
    assert_eq!(code(client.query("SELECT pg_sleep($1)", &[&5f64])), "57014");
    assert!(matches!(
        client.query("SELECT '\0'", &[]),
        Err(Error::Request(_))
    ));
    assert!(matches!(
        client.batch_execute("SELECT '\0'"),
        Err(Error::Request(_))
    ));
    assert!(!client.is_closed());
    assert_eq!(
        client
            .query_one("SELECT count(*) FROM named", &[])
            .unwrap()
            .get::<_, i64>(0),
        1
    );
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn a_connection_the_server_ended_reports_closed() {
    let mut client = connect();
    let mut other = connect();
    let pid: i32 = client
        .query_one("SELECT pg_backend_pid()", &[])
        .unwrap()
        .get(0);
    assert!(
        other
            .query_one("SELECT pg_terminate_backend($1)", &[&pid])
            .unwrap()
            .get::<_, bool>(0)
    );
    // the server announces the termination, then hangs up
    let error = client.query_one("SELECT 1", &[]).unwrap_err();
    assert!(
        matches!(&error, Error::Io(_)) || error.code() == Some("57P01"),
        "{error:?}"
    );
    assert!(client.is_closed());
    assert!(matches!(
        client.query_one("SELECT 1", &[]),
        Err(Error::Closed)
    ));
    assert!(matches!(
        client.batch_execute("SELECT 1"),
        Err(Error::Closed)
    ));
    assert!(matches!(client.transaction().map(drop), Err(Error::Closed)));
    assert!(!other.is_closed());
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn a_block_open_when_the_server_goes_away_does_not_hang() {
    let mut client = connect();
    let mut other = connect();
    let mut tx = client.transaction().unwrap();
    let pid: i32 = tx.query_one("SELECT pg_backend_pid()", &[]).unwrap().get(0);
    other
        .query_one("SELECT pg_terminate_backend($1)", &[&pid])
        .unwrap();
    assert!(tx.execute("SELECT 1", &[]).is_err());
    assert!(tx.commit().is_err());
    assert!(client.is_closed());
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn copy_is_refused_and_ends_the_connection() {
    let mut client = connect();
    assert!(matches!(
        client.batch_execute("COPY (SELECT 1) TO STDOUT"),
        Err(Error::Protocol(_))
    ));
    assert!(client.is_closed());
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn connecting_fails_cleanly() {
    let mut config: alt_pg::Config = crate::admin().parse().unwrap();
    config.set_dbname("alt_pg_no_such_database");
    assert_eq!(code(config.connect().map(drop)), "3D000");
    let absent: alt_pg::Config = "host=/nonexistent-alt-pg user=nobody".parse().unwrap();
    assert!(matches!(absent.connect().map(drop), Err(Error::Io(_))));
    assert!(matches!(
        alt_pg::Client::connect("host=/tmp").map(drop),
        Err(Error::Config(_))
    ));
}
