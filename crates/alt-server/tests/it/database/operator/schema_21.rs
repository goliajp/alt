//! Upgrading an instance of schema 21, which did not record when a request or a
//! credential ended; and what makes a database the schema 20 or 21 one it stands
//! in for.
use super::maintenance::{encryption_key, prepare, publish};
use super::upgrade::{copies, delivered_receipt, observed};
use super::*;

mod served;

/// makes the database the schema 21 one it stands in for
fn as_schema_21(db: &OperatorDb) {
    Client::connect(&db.dsn)
        .unwrap()
        .batch_execute(
            "DELETE FROM alt_schema WHERE version>=22;
             DROP INDEX IF EXISTS alt_requests_manifest_entries;
             DROP INDEX IF EXISTS alt_archived_publications_manifest_entries;
             ALTER TABLE alt_requests DROP CONSTRAINT IF EXISTS alt_requests_settled_at;
             DROP INDEX IF EXISTS alt_requests_settled;
             DROP INDEX IF EXISTS alt_requests_unsettled;
             DROP INDEX IF EXISTS alt_requests_open;
             ALTER TABLE alt_requests DROP COLUMN IF EXISTS settled_at;
             ALTER TABLE alt_credentials DROP CONSTRAINT IF EXISTS alt_credentials_revoked_at;
             ALTER TABLE alt_credentials DROP COLUMN IF EXISTS revoked_at;
             DROP INDEX IF EXISTS alt_segments_request;
             CREATE INDEX IF NOT EXISTS alt_segments_owner ON alt_segments(repository,epoch,actor_id,request_id) WHERE state='uploading'",
        )
        .unwrap();
}
/// makes the database the schema 20 one it stands in for
pub(super) fn as_schema_20(db: &OperatorDb) {
    as_schema_21(db);
    Client::connect(&db.dsn)
        .unwrap()
        .batch_execute(
            "DELETE FROM alt_schema WHERE version=21;
             ALTER TABLE alt_refs DROP CONSTRAINT IF EXISTS alt_refs_repository_fkey;
             ALTER TABLE alt_refs ADD CONSTRAINT alt_refs_repository_fkey FOREIGN KEY (repository) REFERENCES alt_repositories(id)",
        )
        .unwrap();
}
/// whether refs still have a foreign key to their repository
pub(super) fn refs_foreign_key(dsn: &str) -> bool {
    Client::connect(dsn)
        .unwrap()
        .query_one(
            "SELECT EXISTS(SELECT 1 FROM pg_constraint WHERE conrelid='alt_refs'::regclass AND contype='f')",
            &[],
        )
        .unwrap()
        .get(0)
}
/// How many requests have the moment they ended and how many credentials the
/// moment they were revoked; nothing where the database records neither.
fn moments(dsn: &str) -> Option<(i64, i64)> {
    let mut client = Client::connect(dsn).unwrap();
    let recorded: bool = client
        .query_one(
            "SELECT EXISTS(SELECT 1 FROM information_schema.columns WHERE table_name='alt_requests' AND column_name='settled_at')",
            &[],
        )
        .unwrap()
        .get(0);
    recorded.then(|| {
        let row = client
            .query_one(
                "SELECT (SELECT count(*) FROM alt_requests WHERE settled_at IS NOT NULL),(SELECT count(*) FROM alt_credentials WHERE revoked_at IS NOT NULL)",
                &[],
            )
            .unwrap();
        (row.get(0), row.get(1))
    })
}

#[test]
#[ignore = "requires isolated PostgreSQL, age and real operator subprocesses; exercised by closing gate"]
fn a_schema_21_instance_upgrades_in_place_and_from_its_backup() {
    let source = OperatorDb::new();
    let (mut store, admin, _) = prepare(&source);
    let token = alt_server::operator::read_token(std::path::Path::new(&admin)).unwrap();
    let p = publish(&mut store, &token);
    let key = p.publication.key.clone();
    drop(store);
    let config = source.config.to_str().unwrap().to_owned();
    let (before, receipt) = observed(&source, &config, &admin, &key);
    let trust = source.ok(&["health"])["receipt_public_key"]
        .as_str()
        .unwrap()
        .to_owned();
    as_schema_21(&source);
    assert_eq!(copies(&source.dsn), (21, 0, 0));
    assert_eq!(moments(&source.dsn), None);
    assert!(!source.run(&["health"]).status.success());
    // the trees a push of this version refuses are listed before the upgrade,
    // with no credential, and nothing is changed
    let refused = source.ok(&["refused-trees"]);
    assert_eq!(refused["refused_trees"], 0, "{refused}");
    assert!(refused["repositories"].as_u64() >= Some(1), "{refused}");
    assert_eq!(copies(&source.dsn), (21, 0, 0));

    let offsite = alt_os::temp::tempdir().unwrap();
    let (identity, recipient) = encryption_key(offsite.path());
    let backup = source.path("backup");
    let created = source.ok(&[
        "backup",
        "--schema",
        "21",
        "--recipient",
        &recipient,
        "--output",
        &backup,
    ]);
    assert_eq!(created["backup_created"], true);

    // in place, told the proxies in front of it as the package tells its container
    let next = source.path("next.json");
    let migrate = ["migrate", "--from-schema", "21", "--output-config", &next];
    let mut told = source.command();
    told.env("ALT_TRUSTED_PROXIES", "127.0.0.1,::1,172.30.91.1");
    let migrated = told.args(migrate).output().unwrap();
    let said = String::from_utf8_lossy(&migrated.stderr);
    assert!(migrated.status.success(), "{said}");
    let migrated: alt_json::Value = alt_json::from_slice(&migrated.stdout).unwrap();
    assert_eq!(migrated["migrated"], true);
    assert_eq!(migrated["schema"], 23);
    assert_eq!(copies(&source.dsn), (23, 0, 0));
    // The configuration from before had no memory for the packs of pushes told,
    // which is 4096 MiB. The next one has what goes with the memory of the
    // container of the package, which this process is told no other than.
    let written: alt_json::Value = alt_json::from_slice(&std::fs::read(&next).unwrap()).unwrap();
    let megabytes = written["http"]["indexing_megabytes"].as_u64();
    assert!(
        megabytes.is_some_and(|megabytes| megabytes <= 1536),
        "{megabytes:?}"
    );
    // it named no proxy, and the next one names those it was told
    assert_eq!(
        written["http"]["trusted_proxies"],
        alt_json::json!(["127.0.0.1", "::1", "172.30.91.1"])
    );
    // the request is given its moment by the server as it runs, and counted until then
    assert_eq!(moments(&source.dsn), Some((0, 0)));
    let upgraded = Command::new(env!("CARGO_BIN_EXE_alt-server"))
        .arg("--config")
        .arg(&next)
        .env("ALT_OPERATOR_DATABASE", &source.dsn)
        .arg("health")
        .output()
        .unwrap();
    assert!(
        upgraded.status.success(),
        "{}",
        String::from_utf8_lossy(&upgraded.stderr)
    );
    let health: alt_json::Value = alt_json::from_slice(&upgraded.stdout).unwrap();
    assert_eq!(health["requests_awaiting_settled_at"], 1);
    assert_eq!(
        observed(&source, &next, &admin, &key),
        (before, receipt.clone())
    );

    // from the backup taken before the upgrade
    let target = OperatorDb::new();
    let mut config: alt_json::Value =
        alt_json::from_slice(&std::fs::read(&target.config).unwrap()).unwrap();
    config["instance"]["runtime_generation"] = alt_json::json!(10);
    std::fs::write(&target.config, alt_json::to_vec(&config).unwrap()).unwrap();
    let restored = target.ok(&[
        "restore",
        "--input",
        &backup,
        "--identity",
        &identity,
        "--trusted-receipt-key",
        &trust,
        "--maximum-bytes",
        "100000000",
        "--source-fenced",
    ]);
    assert_eq!(restored["restored"], true);
    assert_eq!(
        restored["catalog"]["repositories"][0]["refs"]["refs/heads/main"],
        p.commit.to_string()
    );
    assert_eq!(copies(&target.dsn), (23, 0, 0));
    // the restore revoked every credential of the source, at its own moment
    assert_eq!(moments(&target.dsn).map(|(requests, _)| requests), Some(0));
    let health = target.ok(&["health"]);
    assert_eq!(
        (&health["ready"], &health["requests_awaiting_settled_at"]),
        (&alt_json::json!(true), &alt_json::json!(1))
    );
    assert_eq!(
        delivered_receipt(&target, target.config.to_str().unwrap()),
        receipt
    );
}
