use crate::{admin, code, connect};
use alt_pg::{Config, Error};
use std::time::{Duration, Instant};

const ROLE: &str = "alt_pg_scram";
const PASSWORD: &str = "correct horse 'battery' \\staple";

/// a role that must authenticate with SCRAM, whatever the server's defaults
fn scram_role() -> Config {
    let mut config: Config = admin().parse().unwrap();
    if config.password().is_some() {
        // the server already asks this role for its password
        return config;
    }
    let mut client = connect();
    let quoted = PASSWORD.replace('\'', "''");
    client
        .batch_execute(&format!(
            "SET password_encryption='scram-sha-256'; DROP ROLE IF EXISTS {ROLE}; \
             CREATE ROLE {ROLE} LOGIN PASSWORD '{quoted}'"
        ))
        .unwrap();
    let path: String = client.query_one("SHOW hba_file", &[]).unwrap().get(0);
    let rules = std::fs::read_to_string(&path).unwrap();
    std::fs::write(
        &path,
        format!("local all {ROLE} scram-sha-256\nhost all {ROLE} all scram-sha-256\n{rules}"),
    )
    .unwrap();
    assert!(
        client
            .query_one("SELECT pg_reload_conf()", &[])
            .unwrap()
            .get::<_, bool>(0)
    );
    config.set_user(ROLE).set_password("not the password");
    // the reload is asynchronous: wait until a wrong password is refused
    let deadline = Instant::now() + Duration::from_secs(10);
    while config.connect().is_ok() {
        assert!(Instant::now() < deadline, "server kept trusting {ROLE}");
        std::thread::sleep(Duration::from_millis(20));
    }
    config.set_password(PASSWORD);
    config
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn scram_accepts_the_password_and_refuses_others() {
    let config = scram_role();
    let mut client = config.connect().unwrap();
    let user: String = client
        .query_one("SELECT current_user::text", &[])
        .unwrap()
        .get(0);
    assert_eq!(Some(user.as_str()), config.user());
    let mut wrong = config.clone();
    wrong.set_password("a different password");
    assert_eq!(code(wrong.connect().map(drop)), "28P01");
    let mut unsupported = config.clone();
    unsupported.set_password("pässword");
    assert!(matches!(
        unsupported.connect().map(drop),
        Err(Error::Authentication(_))
    ));
    let parameters = format!(
        "{} user={} dbname=postgres",
        admin()
            .split_whitespace()
            .filter(|part| part.starts_with("host=") || part.starts_with("port="))
            .collect::<Vec<_>>()
            .join(" "),
        config.user().unwrap()
    );
    assert!(matches!(
        alt_pg::Client::connect(&parameters).map(drop),
        Err(Error::Authentication("password missing"))
    ));
    // the first connection is unaffected by the refused ones
    assert_eq!(
        client.query_one("SELECT 1", &[]).unwrap().get::<_, i32>(0),
        1
    );
}
