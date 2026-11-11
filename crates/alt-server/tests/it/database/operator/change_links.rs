use super::*;
use alt_server::change_links::ChannelConfig;
use golia_change_link as link;
use std::net::{IpAddr, Ipv4Addr};

fn start(db: &OperatorDb) -> (ServerProcess, String) {
    let mut child = ServerProcess(
        db.command()
            .arg("serve")
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
        let _ = tx.send(line);
    });
    let line = rx
        .recv_timeout(Duration::from_secs(15))
        .expect("server readiness");
    reader.join().unwrap();
    let ready: alt_json::Value = alt_json::from_str(&line).unwrap();
    (
        child,
        format!("http://{}", ready["address"].as_str().unwrap()),
    )
}
fn channels(url: &str, token: &str) -> alt_json::Value {
    let response = ureq::get(&format!(
        "{url}/v1/repositories/repository/epochs/1/changes/channels"
    ))
    .set("Authorization", &format!("Bearer {token}"))
    .timeout(Duration::from_secs(5))
    .call()
    .unwrap();
    crate::database::json::body(response)
}

#[test]
#[ignore = "requires isolated PostgreSQL and actual operator/server processes"]
fn operator_pairing_requires_explicit_install_and_cannot_resurrect_after_restart() {
    let db = OperatorDb::new();
    let seed = db.path("receipt.key");
    let admin = db.path("admin.token");
    let peer = db.path("peer.token");
    db.ok(&["secret", "receipt-key", "--output", &seed]);
    db.ok(&["secret", "token", "--output", &admin]);
    db.ok(&["secret", "token", "--output", &peer]);
    db.ok(&["initialize"]);
    db.ok(&[
        "bootstrap",
        "owner",
        "--secret",
        &admin,
        "--expires-unix-ms",
        &future().to_string(),
    ]);
    db.ok(&[
        "--credential",
        &admin,
        "repository-create",
        "repository",
        "--quota-bytes",
        "100000000",
    ]);
    let grants_file = db.path("grants.json");
    std::fs::write(
        &grants_file,
        alt_json::to_vec(&alt_json::json!({"read":true,"publish_records":true,"grants":grants()}))
            .unwrap(),
    )
    .unwrap();
    db.ok(&[
        "--credential",
        &admin,
        "grants",
        "repository",
        "owner",
        "--file",
        &grants_file,
    ]);
    let mut config = Config::read(&db.config).unwrap();
    config.change_links = vec![ChannelConfig {
        scope: Scope {
            installation_id: config.instance.installation_id.clone(),
            repository_instance_id: "repository".into(),
            recovery_epoch: 1,
        },
        channel: "explicit-pair".into(),
        application: "app".into(),
        peer: link::Endpoint {
            product: link::Product::Sentori,
            installation: "peer".into(),
            workspace: "space".into(),
            scope: "project".into(),
            generation: link::Counter(1),
        },
        peer_public_key: alt_provenance::encode_hex(
            SigningKey::from_bytes(&[4; 32]).verifying_key().as_bytes(),
        ),
        target: link::transport::Target {
            origin: "http://127.0.0.1:1".into(),
            policy: link::transport::AddressPolicy::Pinned {
                addresses: vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
                allow_plaintext: true,
            },
        },
        peer_browser_origin: Some("https://sentori.example.test".into()),
        local_credential_file: admin.clone().into(),
        peer_credential_file: peer.clone().into(),
    }];
    std::fs::write(&db.config, alt_json::to_vec(&config).unwrap()).unwrap();
    let token = alt_server::operator::read_token(std::path::Path::new(&admin)).unwrap();
    let (child, url) = start(&db);
    assert_eq!(channels(&url, &token), alt_json::json!([]));
    drop(child);
    let install = [
        "--credential",
        &admin,
        "link-install",
        "repository",
        "1",
        "explicit-pair",
    ];
    db.ok(&install);
    db.ok(&install);
    let (child, url) = start(&db);
    assert_eq!(channels(&url, &token)[0]["enabled"], true);
    drop(child);
    // An operator configuration edit alone cannot change an installed pairing.
    config.change_links[0].target.origin = "http://127.0.0.1:2".into();
    std::fs::write(&db.config, alt_json::to_vec(&config).unwrap()).unwrap();
    let (child, url) = start(&db);
    assert_eq!(channels(&url, &token)[0]["enabled"], false);
    drop(child);
    assert!(!db.run(&install).status.success());
    config.change_links[0].target.origin = "http://127.0.0.1:1".into();
    std::fs::write(&db.config, alt_json::to_vec(&config).unwrap()).unwrap();
    db.ok(&[
        "--credential",
        &admin,
        "link-disable",
        "repository",
        "1",
        "explicit-pair",
    ]);
    let (child, url) = start(&db);
    assert_eq!(channels(&url, &token)[0]["enabled"], false);
    assert!(!db.run(&install).status.success());
    drop(child);
    config.change_links[0].channel = "replacement-pair".into();
    std::fs::write(&db.config, alt_json::to_vec(&config).unwrap()).unwrap();
    db.ok(&[
        "--credential",
        &admin,
        "link-install",
        "repository",
        "1",
        "replacement-pair",
    ]);
    let (child, url) = start(&db);
    let rows = channels(&url, &token);
    assert_eq!(
        rows.as_array()
            .unwrap()
            .iter()
            .filter(|r| r["enabled"] == true)
            .count(),
        1
    );
    assert!(
        rows.as_array()
            .unwrap()
            .iter()
            .any(|r| r["channel"] == "replacement-pair" && r["enabled"] == true)
    );
    drop(child);
}
