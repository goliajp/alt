//! Upgrading an instance that kept published Git objects in the content store:
//! in place with `migrate`, and by restoring its backup under the new version.
use super::maintenance::{encryption_key, prepare, publish};
use super::*;

/// Gives the content store a copy of each object the way schema 18 kept published
/// ones, and makes the database the schema 18 one it stands in for.
fn as_schema_18(db: &OperatorDb, store: &Store, objects: &[(&ContentEntry, &Vec<u8>, &str)]) {
    let mut client = Client::connect(&db.dsn, NoTls).unwrap();
    client
        .batch_execute("DELETE FROM alt_schema WHERE version=19; ALTER TABLE alt_pins DROP CONSTRAINT alt_pins_no_ref")
        .unwrap();
    let scope = Scope {
        installation_id: store.spec().installation_id.clone(),
        repository_instance_id: "repository".into(),
        recovery_epoch: 1,
    };
    for (entry, bytes, pin) in objects {
        store
            .bytes()
            .put(
                &scope,
                &entry.blake3,
                entry.bytes,
                limits().object_bytes,
                &mut std::io::Cursor::new(bytes),
            )
            .unwrap();
        let kind = alt_json::to_value(entry.kind).unwrap();
        client.execute("INSERT INTO alt_blobs(repository,blake3,bytes,state,accounted,created_at) VALUES('repository',$1,$2,'stored',true,0) ON CONFLICT(repository,blake3) DO UPDATE SET state='stored',accounted=true", &[&entry.blake3,&(entry.bytes as i64)]).unwrap();
        client
            .execute(
                "UPDATE alt_repositories SET stored_bytes=stored_bytes+$1",
                &[&(entry.bytes as i64)],
            )
            .unwrap();
        client
            .execute(
                "INSERT INTO alt_objects(repository,oid,kind,blake3) VALUES('repository',$1,$2,$3)",
                &[&entry.git_oid, &kind.as_str(), &entry.blake3],
            )
            .unwrap();
        client.execute("INSERT INTO alt_pins(repository,pin_id,blake3,expires_at) VALUES('repository',$1,$2,NULL)", &[pin,&entry.blake3]).unwrap();
    }
}
/// schema, ref pins, object mappings, objects in live segments, and whether the
/// stored bytes are the live segments plus the counted content
fn shape(dsn: &str) -> (i32, i64, i64, i64, bool) {
    let row = Client::connect(dsn, NoTls).unwrap().query_one("SELECT (SELECT max(version) FROM alt_schema),(SELECT count(*) FROM alt_pins WHERE pin_id LIKE 'ref:%'),(SELECT count(*) FROM alt_objects),(SELECT sum(objects) FROM alt_pack_segments WHERE state='live')::bigint,(SELECT stored_bytes FROM alt_repositories WHERE id='repository')=(SELECT sum(bytes) FROM alt_pack_segments WHERE state='live')::bigint+(SELECT COALESCE(sum(bytes),0) FROM alt_blobs WHERE accounted)::bigint AND NOT EXISTS(SELECT 1 FROM alt_blobs WHERE accounted)", &[]).unwrap();
    (row.get(0), row.get(1), row.get(2), row.get(3), row.get(4))
}

#[test]
#[ignore = "requires isolated PostgreSQL, age and real operator subprocesses; exercised by closing gate"]
fn a_schema_18_instance_upgrades_in_place_and_from_its_backup() {
    let source = OperatorDb::new();
    let (mut store, admin, _) = prepare(&source);
    let token = alt_server::operator::read_token(std::path::Path::new(&admin)).unwrap();
    let p = publish(&mut store, &token);
    // an object only the content store has, as old task publications could leave one
    let raw = b"kept outside every pack segment".to_vec();
    let digest = alt_hash::blake3::hash(&raw).to_hex().to_string();
    let loose = ContentEntry {
        kind: ContentKind::Blob,
        blake3: digest.clone(),
        bytes: raw.len() as u64,
        git_oid: Some(ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, &raw).to_string()),
        chunks: vec![],
    };
    let mut objects: Vec<(&ContentEntry, &Vec<u8>, &str)> = p
        .manifest
        .entries
        .iter()
        .zip(&p.contents)
        .map(|(entry, bytes)| (entry, bytes, "ref:refs/heads/main"))
        .collect();
    objects.push((&loose, &raw, "task-bundle:before"));
    let trust = source.ok(&["health"])["receipt_public_key"]
        .as_str()
        .unwrap()
        .to_owned();
    as_schema_18(&source, &store, &objects);
    drop(store);
    assert_eq!(shape(&source.dsn), (18, 2, 3, 2, false));
    assert!(!source.run(&["health"]).status.success());

    let offsite = alt_os::temp::tempdir().unwrap();
    let (identity, recipient) = encryption_key(offsite.path());
    let backup = source.path("backup");
    let created = source.ok(&[
        "backup",
        "--schema",
        "18",
        "--recipient",
        &recipient,
        "--output",
        &backup,
    ]);
    assert_eq!(created["backup_created"], true);

    // in place
    let next = source.path("next.json");
    let migrated = source.ok(&["migrate", "--from-schema", "18", "--output-config", &next]);
    assert_eq!(migrated["migrated"], true);
    assert_eq!(migrated["schema"], 19);
    assert_eq!(shape(&source.dsn), (19, 0, 0, 3, true));
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
    assert_eq!(shape(&target.dsn), (19, 0, 0, 3, true));
    assert_eq!(target.ok(&["health"])["ready"], true);
}

#[test]
#[ignore = "requires isolated PostgreSQL, age and real operator subprocesses; exercised by closing gate"]
fn an_instance_older_than_schema_18_is_told_which_release_upgrades_it() {
    let source = OperatorDb::new();
    let (store, _, _) = prepare(&source);
    drop(store);
    Client::connect(&source.dsn, NoTls)
        .unwrap()
        .batch_execute("DELETE FROM alt_schema WHERE version>17")
        .unwrap();
    let next = source.path("next.json");
    let migrated = source.run(&["migrate", "--from-schema", "17", "--output-config", &next]);
    assert!(!migrated.status.success());
    assert!(
        String::from_utf8_lossy(&migrated.stderr).contains(
            "this release upgrades from schema 18 only; upgrade an older instance to schema 18 with the previous release first"
        ),
        "{}",
        String::from_utf8_lossy(&migrated.stderr)
    );
    assert!(!std::path::Path::new(&next).exists());

    let offsite = alt_os::temp::tempdir().unwrap();
    let (_, recipient) = encryption_key(offsite.path());
    let backup = source.path("backup");
    let created = source.run(&[
        "backup",
        "--schema",
        "17",
        "--recipient",
        &recipient,
        "--output",
        &backup,
    ]);
    assert!(!created.status.success());
    assert!(
        String::from_utf8_lossy(&created.stderr)
            .contains("this release backs up and restores schema 18 and 19 only"),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    assert!(!std::path::Path::new(&backup).exists());
}
