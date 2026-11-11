use super::*;
use golia_platform_contract::{
    Audience, Counter, Product,
    lifecycle::{Images, Probe, ProvisionSpec, Readiness},
};
use std::os::unix::fs::PermissionsExt;

fn input(db: &OperatorDb) -> ProvisionSpec {
    ProvisionSpec {
        version: 1,
        audience: Audience {
            organization_id: "cloud-org".to_string().try_into().unwrap(),
            workspace_id: "shared-space".to_string().try_into().unwrap(),
            installation_id: db.name.clone().try_into().unwrap(),
            product: Product::Alt,
            generation: Counter(1),
            origin: "https://alt.example.test".to_string().try_into().unwrap(),
            control_origin: "https://cloud.example.test".to_string().try_into().unwrap(),
        },
        region: "test-region".to_string().try_into().unwrap(),
        images: Images {
            application: format!("sha256:{}", "ab".repeat(32)),
            database: format!("sha256:{}", "cd".repeat(32)),
            edge: Some(format!("sha256:{}", "ef".repeat(32))),
        },
        runtime_key: "12".repeat(32).try_into().unwrap(),
        data_key: "34".repeat(32).try_into().unwrap(),
        database_password: "56".repeat(32).try_into().unwrap(),
    }
}
#[test]
#[ignore = "requires isolated PostgreSQL, real initializer interruption and runtime HTTP process"]
fn cloud_provision_interrupt_resume_and_storage_binding() {
    let db = OperatorDb::new();
    let root = db.root.path();
    let state = root.join("state");
    let assets = root.join("assets");
    std::fs::create_dir(&state).unwrap();
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::create_dir(&assets).unwrap();
    std::fs::write(
        assets.join("index.html"),
        "<!doctype html><title>Alt</title>",
    )
    .unwrap();
    let spec_path = root.join("spec.json");
    let spec = input(&db);
    alt_server::operator::create_private(&spec_path, &serde_json::to_vec(&spec).unwrap()).unwrap();
    let initialize = || {
        let mut c = db.command();
        c.args([
            "cloud-initialize",
            "--spec",
            spec_path.to_str().unwrap(),
            "--directory",
            state.to_str().unwrap(),
            "--assets",
            assets.to_str().unwrap(),
            "--listen",
            "127.0.0.1:0",
            "--database-environment",
            "ALT_OPERATOR_DATABASE",
        ])
        .env(
            "ALT_OPERATOR_DATABASE",
            format!("{} application_name=alt_cloud_initialize_interrupt", db.dsn),
        );
        c
    };
    let mut blocker = Client::connect(&db.dsn, NoTls).unwrap();
    blocker
        .query_one("SELECT pg_advisory_lock(715071909111)", &[])
        .unwrap();
    let mut interrupted = ServerProcess(
        initialize()
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        let waiting: bool = blocker.query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE application_name='alt_cloud_initialize_interrupt' AND wait_event='advisory')", &[]).unwrap().get(0);
        if waiting {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "initializer did not reach database barrier"
        );
        assert!(
            interrupted.0.try_wait().unwrap().is_none(),
            "initializer exited before barrier"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let saved = [
        "provision.json",
        "runtime.key",
        "receipt.key",
        "config.json",
    ]
    .map(|p| std::fs::read(state.join(p)).unwrap());
    interrupted.0.kill().unwrap();
    interrupted.0.wait().unwrap();
    blocker
        .query_one("SELECT pg_advisory_unlock(715071909111)", &[])
        .unwrap();
    for _ in 0..2 {
        let result = initialize().output().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        for (index, name) in [
            "provision.json",
            "runtime.key",
            "receipt.key",
            "config.json",
        ]
        .iter()
        .enumerate()
        {
            assert_eq!(std::fs::read(state.join(name)).unwrap(), saved[index]);
        }
    }
    let mut generated: serde_json::Value = serde_json::from_slice(&saved[3]).unwrap();
    assert_eq!(generated["portable"]["total_bytes"], "4294967296");
    // An upgrade/restart must not enable a new API or replace a legacy frozen
    // config. Exercise the real initializer with its pre-portable encoding.
    generated.as_object_mut().unwrap().remove("portable");
    let legacy: alt_server::operator::Config = serde_json::from_value(generated).unwrap();
    let legacy_bytes = serde_json::to_vec_pretty(&legacy).unwrap();
    std::fs::write(state.join("config.json"), &legacy_bytes).unwrap();
    assert!(initialize().output().unwrap().status.success());
    assert_eq!(
        std::fs::read(state.join("config.json")).unwrap(),
        legacy_bytes
    );
    // Bounds remain part of a frozen new configuration, not an ignored field.
    let mut changed: serde_json::Value = serde_json::from_slice(&saved[3]).unwrap();
    changed["portable"]["total_bytes"] = serde_json::json!("1");
    std::fs::write(
        state.join("config.json"),
        serde_json::to_vec(&changed).unwrap(),
    )
    .unwrap();
    assert!(!initialize().output().unwrap().status.success());
    std::fs::write(state.join("config.json"), &saved[3]).unwrap();
    let exists: i64 = blocker
        .query_one("SELECT count(*) FROM alt_instance", &[])
        .unwrap()
        .get(0);
    assert_eq!(exists, 1);
    let mut wrong = spec.clone();
    wrong.data_key = "78".repeat(32).try_into().unwrap();
    std::fs::write(&spec_path, serde_json::to_vec(&wrong).unwrap()).unwrap();
    assert!(!initialize().output().unwrap().status.success());
    std::fs::write(&spec_path, serde_json::to_vec(&spec).unwrap()).unwrap();
    let config = state.join("config.json");
    // Pairing settings survive the exact initializer used by cloud-serve;
    // frozen provision/config/key bytes retain their original identities.
    let frozen: alt_server::operator::Config = serde_json::from_slice(&saved[3]).unwrap();
    let mut channel = crate::cloud_settings::channel();
    channel["scope"]["installation_id"] = serde_json::json!(frozen.instance.installation_id);
    alt_server::operator::create_private(
        &state.join("local.key"),
        format!("alt1_{}", "ab".repeat(32)).as_bytes(),
    )
    .unwrap();
    alt_server::operator::create_private(&state.join("peer.key"), b"peer-token").unwrap();
    let settings = serde_json::json!({"version":1,"instance":frozen.instance,"channels":[channel]});
    alt_server::operator::create_private(
        &state.join("change-links.json"),
        &serde_json::to_vec(&settings).unwrap(),
    )
    .unwrap();
    for _ in 0..2 {
        assert!(initialize().output().unwrap().status.success());
        assert_eq!(std::fs::read(&config).unwrap(), saved[3]);
        let loaded = alt_server::operator::Config::read(&config).unwrap();
        assert_eq!(loaded.change_links.len(), 1);
        assert_eq!(
            loaded.change_links[0].scope.installation_id,
            spec.audience.installation_id.as_str()
        );
    }
    let mut child = ServerProcess(
        Command::new(env!("CARGO_BIN_EXE_alt-server"))
            .args(["--config", config.to_str().unwrap(), "serve"])
            .env("ALT_OPERATOR_DATABASE", &db.dsn)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let stdout = child.0.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut line = String::new();
        BufReader::new(stdout).read_line(&mut line).unwrap();
        tx.send(line).unwrap();
    });
    let line = rx.recv_timeout(Duration::from_secs(15)).unwrap();
    reader.join().unwrap();
    let started: serde_json::Value = serde_json::from_str(&line).unwrap();
    let url = format!(
        "http://{}/cloud/runtime/v1/health",
        started["address"].as_str().unwrap()
    );
    let probe = Probe {
        recovery_manifest_digest: None,
        version: 1,
        audience: spec.audience.clone(),
        spec_digest: spec.digest().unwrap(),
        challenge: "90".repeat(32),
    };
    let call = |probe: &Probe, key: &str, cookie: bool| {
        let mut req = ureq::post(&url)
            .timeout(Duration::from_secs(5))
            .set("Authorization", &format!("Bearer {key}"))
            .set("Content-Type", "application/json");
        if cookie {
            req = req.set("Cookie", "session=unrelated");
        }
        req.send_string(&serde_json::to_string(probe).unwrap())
            .map_err(|error| match error {
                ureq::Error::Status(code, _) => code,
                error => panic!("probe transport: {error}"),
            })
    };
    let result: Readiness =
        crate::database::json::body(call(&probe, spec.runtime_key.expose(), false).unwrap());
    result.check(&probe).unwrap();
    assert!(matches!(
        call(&probe, spec.runtime_key.expose(), true),
        Err(401)
    ));
    assert!(matches!(call(&probe, &"ab".repeat(32), false), Err(401)));
    let mut stale = probe.clone();
    stale.audience.generation.0 += 1;
    assert!(matches!(
        call(&stale, spec.runtime_key.expose(), false),
        Err(403)
    ));
    let marker_path = state.join("bytes/instance.json");
    let marker = std::fs::read(&marker_path).unwrap();
    std::fs::write(&marker_path, b"{}").unwrap();
    assert!(matches!(
        call(&probe, spec.runtime_key.expose(), false),
        Err(503)
    ));
    std::fs::write(&marker_path, marker).unwrap();
    blocker
        .execute(
            "UPDATE alt_cloud_binding SET provision_digest=$1",
            &[&"ab".repeat(32)],
        )
        .unwrap();
    assert!(matches!(
        call(&probe, spec.runtime_key.expose(), false),
        Err(503)
    ));
    blocker
        .execute(
            "UPDATE alt_cloud_binding SET provision_digest=$1",
            &[&spec.digest().unwrap()],
        )
        .unwrap();
    assert!(call(&probe, spec.runtime_key.expose(), false).is_ok());
}
