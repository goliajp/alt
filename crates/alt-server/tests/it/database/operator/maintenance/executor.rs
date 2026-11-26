//! A Cloud instance restored by the command its executor runs.
use super::*;

/// The command the executor runs to restore a Cloud instance into a place it
/// manages, with the backup of `config`.
pub(super) fn recovered_by_the_executor(
    config: &Config,
    backup: &std::path::Path,
    identity: &str,
    trust: String,
) {
    use golia_platform_contract::Counter;
    let input = backup.to_str().unwrap();
    // The executor-facing command reconciles a lost completion response without
    // importing the archive again or overwriting a completed target.
    use golia_platform_contract::lifecycle::{Images, ProvisionSpec, RecoveryBackup, RecoverySpec};
    let beside = alt_server::operator::backup::beside(backup).unwrap();
    let verified = alt_server::operator::backup::verify(
        (backup, beside),
        std::path::Path::new(&identity),
        config.signing_key().unwrap().verifying_key().to_bytes(),
        100_000_000,
    )
    .unwrap();
    let source_spec = ProvisionSpec {
        version: 1,
        audience: config.cloud.as_ref().unwrap().audience.clone(),
        region: "region".to_string().try_into().unwrap(),
        images: Images {
            application: format!("sha256:{}", "a".repeat(64)),
            database: format!("sha256:{}", "b".repeat(64)),
            edge: Some(format!("sha256:{}", "c".repeat(64))),
        },
        runtime_key: "ab".repeat(32).try_into().unwrap(),
        data_key: "01".repeat(32).try_into().unwrap(),
        database_password: "02".repeat(32).try_into().unwrap(),
    };
    let mut next_spec = source_spec.clone();
    next_spec.audience.generation = Counter(20);
    next_spec.runtime_key = "03".repeat(32).try_into().unwrap();
    next_spec.data_key = "04".repeat(32).try_into().unwrap();
    next_spec.database_password = "05".repeat(32).try_into().unwrap();
    let recovery = RecoverySpec {
        version: 1,
        operation_id: "restore-operation".to_string().try_into().unwrap(),
        source: source_spec,
        target: next_spec.clone(),
        backup: RecoveryBackup {
            manifest_digest: verified.manifest_digest,
            receipt_public_key: trust,
            generation: Counter(1),
            maximum_bytes: Counter(100_000_000),
        },
    };
    let managed = OperatorDb::new();
    let state = managed.root.path().join("state");
    let assets = managed.root.path().join("assets");
    std::fs::create_dir(&state).unwrap();
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::create_dir(&assets).unwrap();
    std::fs::write(
        assets.join("index.html"),
        "<!doctype html><title>Alt</title>",
    )
    .unwrap();
    let spec_path = managed.root.path().join("recovery.json");
    alt_server::operator::create_private(
        &spec_path,
        &alt_json::to_vec(&alt_json_serde::Serde(&recovery)).unwrap(),
    )
    .unwrap();
    let restore_args = [
        "cloud-restore",
        "--spec",
        spec_path.to_str().unwrap(),
        "--directory",
        state.to_str().unwrap(),
        "--assets",
        assets.to_str().unwrap(),
        "--database-environment",
        "ALT_OPERATOR_DATABASE",
        "--input",
        input,
        "--identity",
        identity,
        "--source-fenced",
    ];
    // A restoration that fails before it activates the instance leaves nothing
    // of itself: the same command is given again and restores.
    let mut db = Client::connect(&managed.dsn).unwrap();
    db.batch_execute("CREATE SCHEMA held; CREATE FUNCTION held.refuse() RETURNS event_trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'no table is made here'; END $$; CREATE EVENT TRIGGER refuse_tables ON ddl_command_start WHEN TAG IN ('CREATE TABLE') EXECUTE FUNCTION held.refuse()").unwrap();
    let failed = managed.run(&restore_args);
    let said = String::from_utf8_lossy(&failed.stderr).into_owned()
        + &String::from_utf8_lossy(&failed.stdout);
    assert_eq!(failed.status.code(), Some(1), "{said}");
    assert!(said.contains("nothing is left of it"), "{said}");
    db.batch_execute("DROP EVENT TRIGGER refuse_tables; DROP SCHEMA held CASCADE")
        .unwrap();
    for _ in 0..2 {
        let outcome = managed.ok(&restore_args);
        assert_eq!(outcome["instance"]["runtime_generation"], 20);
        assert_eq!(outcome["manifest_digest"], recovery.backup.manifest_digest);
    }
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM alt_instance_audit WHERE action='instance.restore'",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        1
    );
    let provision_path = state.join("provision.json");
    let start_args = [
        "cloud-initialize",
        "--spec",
        provision_path.to_str().unwrap(),
        "--directory",
        state.to_str().unwrap(),
        "--assets",
        assets.to_str().unwrap(),
        "--database-environment",
        "ALT_OPERATOR_DATABASE",
    ];
    managed.ok(&start_args);
    let cfg = Config::read(&state.join("config.json")).unwrap();
    let mut recovered = Store::open(
        Client::connect(&managed.dsn).unwrap(),
        ByteStore::open(&cfg.bytes_directory).unwrap(),
        cfg.instance.clone(),
        cfg.signing_key().unwrap(),
        limits(),
    )
    .unwrap();
    let runtime = cfg.cloud_runtime().unwrap().unwrap();
    let mut probe = golia_platform_contract::lifecycle::Probe {
        version: 1,
        audience: next_spec.audience.clone(),
        spec_digest: next_spec.digest().unwrap(),
        challenge: "f".repeat(64),
        recovery_manifest_digest: Some(recovery.backup.manifest_digest.clone()),
    };
    runtime.readiness(&mut recovered, probe.clone()).unwrap();
    probe.recovery_manifest_digest = Some("a".repeat(64));
    assert!(runtime.readiness(&mut recovered, probe).is_err());
    db.execute(
        "DELETE FROM alt_instance_audit WHERE action='instance.restore'",
        &[],
    )
    .unwrap();
    assert!(!managed.run(&restore_args).status.success());
    assert!(!managed.run(&start_args).status.success());
}
