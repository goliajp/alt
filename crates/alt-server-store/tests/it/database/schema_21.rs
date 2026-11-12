//! Schema 21 drops the foreign key from a ref to its repository. These hold the
//! upgrade of an instance that still has it.
use super::*;

const FOREIGN_KEY: &str = "alt_refs_repository_fkey";

/// makes the database the schema 20 one it stands in for; harmless on one that is
pub(super) fn as_schema_20(f: &Fixture) {
    f.client()
        .batch_execute(&format!(
            "DELETE FROM alt_schema WHERE version=21;
             ALTER TABLE alt_refs DROP CONSTRAINT IF EXISTS {FOREIGN_KEY};
             ALTER TABLE alt_refs ADD CONSTRAINT {FOREIGN_KEY} FOREIGN KEY (repository) REFERENCES alt_repositories(id)"
        ))
        .unwrap();
}

fn texts(f: &Fixture, sql: &str) -> Vec<String> {
    f.client()
        .query(sql, &[])
        .unwrap()
        .iter()
        .map(|row| row.get(0))
        .collect()
}
fn foreign_keys(f: &Fixture) -> Vec<String> {
    texts(
        f,
        "SELECT conname::text FROM pg_constraint WHERE conrelid='alt_refs'::regclass AND contype='f'",
    )
}
fn refs(f: &Fixture) -> Vec<String> {
    texts(
        f,
        "SELECT repository||' '||name||' '||oid FROM alt_refs ORDER BY repository,name",
    )
}
/// schema version and runtime generation
fn version(f: &Fixture) -> (i32, i64) {
    let row = f
        .client()
        .query_one(
            "SELECT (SELECT max(version) FROM alt_schema),(SELECT runtime_generation FROM alt_instance)",
            &[],
        )
        .unwrap();
    (row.get(0), row.get(1))
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn a_new_database_has_no_foreign_key_on_refs() {
    let mut f = Fixture::new();
    assert_eq!(foreign_keys(&f), Vec::<String>::new());
    Payload::new("first", None).publish_new(&mut f.store, &f.credential);
    assert_eq!(refs(&f).len(), 1);
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn an_older_instance_loses_the_foreign_key_and_keeps_its_refs() {
    for from in [20, 19, 18] {
        let mut f = Fixture::new();
        let first = Payload::new("first", None);
        first.publish_new(&mut f.store, &f.credential);
        let second = Payload::new("second", Some(first.commit));
        second.publish_new(&mut f.store, &f.credential);
        let before = refs(&f);
        assert!(!before.is_empty());

        match from {
            20 => as_schema_20(&f),
            _ => super::schema_20::as_schema_19(&f),
        }
        if from == 18 {
            f.client()
                .batch_execute("DELETE FROM alt_schema WHERE version=19; ALTER TABLE alt_pins DROP CONSTRAINT alt_pins_no_ref")
                .unwrap();
        }
        assert_eq!(version(&f), (from as i32, 1));
        assert_eq!(foreign_keys(&f), [FOREIGN_KEY]);
        assert!(f.store.check_connection().is_err());

        super::schema_20::migrate(&mut f, from).unwrap();
        assert_eq!(version(&f), (21, 2), "from schema {from}");
        assert_eq!(foreign_keys(&f), Vec::<String>::new());
        assert_eq!(refs(&f), before);
        f.store.check_connection().unwrap();
        // the upgraded instance goes on publishing
        Payload::new("third", Some(second.commit)).publish_new(&mut f.store, &f.credential);
        assert_ne!(refs(&f), before);
        // and is not a source any more
        assert!(matches!(
            super::schema_20::migrate(&mut f, from),
            Err(alt_server_store::Error::Precondition(_))
        ));
    }
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn a_schema_20_database_without_the_foreign_key_is_left_as_it_is() {
    let mut f = Fixture::new();
    Payload::new("first", None).publish_new(&mut f.store, &f.credential);
    as_schema_20(&f);
    f.client()
        .batch_execute(&format!(
            "ALTER TABLE alt_refs DROP CONSTRAINT {FOREIGN_KEY}"
        ))
        .unwrap();
    assert!(super::schema_20::migrate(&mut f, 20).is_err());
    assert_eq!(version(&f), (20, 1));
}
