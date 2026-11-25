//! An instance whose segments do not give every object back, backed up and
//! restored.
use super::*;
use alt_json::{Value, json};

/// What a command told and how it ended.
fn told(db: &OperatorDb, args: &[&str]) -> (Option<i32>, Value) {
    let out = db.run(args);
    // what a command that told nothing said instead
    let said = || Value::String(String::from_utf8_lossy(&out.stderr).into_owned());
    let told = alt_json::from_slice(&out.stdout).unwrap_or_else(|_| said());
    (out.status.code(), told)
}

/// A destination for a restoration: nothing in its database, no files.
fn destination() -> OperatorDb {
    let target = OperatorDb::new();
    let mut config: Value = alt_json::from_slice(&std::fs::read(&target.config).unwrap()).unwrap();
    config["instance"]["runtime_generation"] = json!(10);
    std::fs::write(&target.config, alt_json::to_vec(&config).unwrap()).unwrap();
    target
}

/// How many tables, views and sequences the database of `db` has.
fn relations(db: &OperatorDb) -> i64 {
    let count = "SELECT count(*) FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname !~ '^pg_' AND n.nspname<>'information_schema'";
    let mut client = Client::connect(&db.dsn).unwrap();
    client.query_one(count, &[]).unwrap().get(0)
}

/// A segment whose index leads the read of one object to another is a file
/// like any other to a backup that holds files to their digests. The instance
/// is read through before it is backed up: it is not backed up as it is, and
/// the operator is told which segment and which object. Asked for as that, the
/// backup is made and names the segment, and a restoration of it tells of the
/// segment again. A backup that does not name it, as one made before objects
/// were read, is not restored: what the restoration wrote is taken away, and
/// the same destination takes the next restoration. Asked for, it is restored
/// as it is. Mended, the instance is backed up as any other.
#[test]
#[ignore = "requires isolated PostgreSQL, matching pg_dump/pg_restore and age tools; real operator backups and restorations"]
fn an_instance_whose_segments_do_not_give_their_objects_back_is_backed_up_and_restored_as_that_alone()
 {
    let db = OperatorDb::new();
    let (mut store, admin, _) = maintenance::prepare(&db);
    let token = alt_server::operator::read_token(std::path::Path::new(&admin)).unwrap();
    maintenance::push_large(&mut store, &token);
    drop(store);
    let trust = db.ok(&["health"])["receipt_public_key"]
        .as_str()
        .unwrap()
        .to_owned();
    let files = maintenance::segment_files(&Config::read(&db.config).unwrap().bytes_directory);
    // the first record of the index tells of the entry of the second
    let mut bytes = std::fs::read(&files[0]).unwrap();
    let end = bytes.len() - 64;
    let pack = u64::from_le_bytes(bytes[end..end + 8].try_into().unwrap()) as usize;
    let records = pack + 1024;
    for field in [52..60, 68..76] {
        let other = bytes[records + 88..][field.clone()].to_vec();
        bytes[records..][field].copy_from_slice(&other);
    }
    let sum = alt_hash::blake3::hash(&bytes[pack..end]);
    bytes[end + 24..end + 56].copy_from_slice(sum.as_bytes());
    std::fs::write(&files[0], &bytes).unwrap();

    let offsite = alt_os::temp::tempdir().unwrap();
    let (identity, recipient) = maintenance::encryption_key(offsite.path());
    let backup = |to: &str, more: &[&str]| {
        let to = db.path(to);
        let args = ["backup", "--recipient", &recipient, "--output", &to];
        told(&db, &[&args[..], more].concat())
    };
    // what the operator gave is looked at before the objects are read
    let unsent = db.run(&[
        "backup",
        "--recipient",
        "nobody",
        "--output",
        &db.path("first"),
    ]);
    let why = String::from_utf8_lossy(&unsent.stderr).into_owned();
    assert!(why.contains("age X25519 recipient"), "{why}");
    let (code, refused) = backup("first", &[]);
    assert_eq!((code, &refused["backup_created"]), (Some(1), &json!(false)));
    let unread = refused["unread"].as_array().unwrap();
    assert_eq!(unread.len(), 1, "{refused}");
    assert_eq!(unread[0]["repository"], "repository");
    let segment = unread[0]["segment"].as_str().unwrap().to_owned();
    assert!(files[0].to_str().unwrap().contains(&segment), "{refused}");
    assert_eq!(unread[0]["objects"], 1, "{refused}");
    assert_eq!(unread[0]["first"][0][0].as_str().unwrap().len(), 40);
    let hint = refused["message"].as_str().unwrap();
    assert!(hint.contains("scrub --mend") && hint.contains("--with-unread"));
    assert!(!std::path::Path::new(&db.path("first")).exists());

    let (code, made) = backup("named", &["--with-unread"]);
    assert_eq!((code, &made["backup_created"]), (Some(0), &json!(true)));
    assert_eq!(made["unread"][0]["segment"], segment.as_str(), "{made}");
    // the same backup as one that names no segment, signed by the same key
    let unnamed = db.path("unnamed");
    std::fs::create_dir(&unnamed).unwrap();
    let named = std::path::Path::new(&db.path("named")).to_owned();
    std::fs::copy(
        named.join("payload.tar.age"),
        format!("{unnamed}/payload.tar.age"),
    )
    .unwrap();
    let signed: Value =
        alt_json::from_slice(&std::fs::read(named.join("manifest.json")).unwrap()).unwrap();
    let mut manifest: alt_server::operator::backup::Manifest =
        alt_json::from_slice(&alt_json::to_vec(&signed["manifest"]).unwrap()).unwrap();
    assert_eq!(manifest.unread.len(), 1);
    manifest.unread.clear();
    let key: [u8; 32] = std::fs::read(db.path("receipt.key"))
        .unwrap()
        .try_into()
        .unwrap();
    let signature = SigningKey::from_bytes(&key).sign(&alt_json::to_vec(&manifest).unwrap());
    let signature = alt_base64::STANDARD.encode(signature.to_bytes());
    let resigned = json!({"manifest": manifest, "signature": signature});
    std::fs::write(
        format!("{unnamed}/manifest.json"),
        alt_json::to_vec(&resigned).unwrap(),
    )
    .unwrap();

    let restore = |target: &OperatorDb, from: &str, more: &[&str]| {
        let from = db.path(from);
        let args = ["restore", "--input", &from, "--identity", &identity];
        let trusted = [
            "--trusted-receipt-key",
            &trust,
            "--maximum-bytes",
            "100000000",
        ];
        told(
            target,
            &[&args[..], &trusted[..], &["--source-fenced"], more].concat(),
        )
    };
    let target = destination();
    let bytes_directory = Config::read(&target.config).unwrap().bytes_directory;
    let (code, refused) = restore(&target, "unnamed", &[]);
    assert_eq!(
        (code, &refused["restored"]),
        (Some(1), &json!(false)),
        "{refused}"
    );
    assert_eq!(
        refused["unread"][0]["segment"],
        segment.as_str(),
        "{refused}"
    );
    assert!(
        refused["message"]
            .as_str()
            .unwrap()
            .contains("--with-unread")
    );
    // nothing is left of it, and the same destination takes the next
    assert!(!bytes_directory.exists());
    assert!(!std::path::Path::new(&target.path("receipt.key")).exists());
    assert_eq!(relations(&target), 0);
    let (code, restored) = restore(&target, "named", &[]);
    assert_eq!(
        (code, &restored["restored"]),
        (Some(0), &json!(true)),
        "{restored}"
    );
    assert_eq!(
        restored["unread"][0]["segment"],
        segment.as_str(),
        "{restored}"
    );
    assert_eq!(target.ok(&["health"])["ready"], true);

    let target = destination();
    let (code, restored) = restore(&target, "unnamed", &["--with-unread"]);
    assert_eq!(
        (code, &restored["restored"]),
        (Some(0), &json!(true)),
        "{restored}"
    );
    assert_eq!(restored["unread"][0]["objects"], 1, "{restored}");
    assert_eq!(target.ok(&["health"])["ready"], true);

    let mended = db.ok(&["--credential", &admin, "scrub", "--mend"]);
    assert_eq!(mended["unread_objects"], 0, "{mended}");
    let (code, made) = backup("mended", &[]);
    assert_eq!(
        (code, &made["backup_created"]),
        (Some(0), &json!(true)),
        "{made}"
    );
    assert_eq!(made["unread"], json!([]), "{made}");
}
