use alt_server::operator::{Config, create_private};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::Path,
};

fn fixture(root: &Path) -> Value {
    let mut c: Value = serde_json::from_str(include_str!("../../example-config.json")).unwrap();
    c["instance"] = json!({"installation_id":"installation", "workspace_id":"workspace",
        "runtime_generation":1,"deployment_mode":"cloud"});
    c["browser"] = json!({"public_origin":"https://alt.example.test",
        "assets_directory":"web","session_ttl_ms":60000});
    c["cloud"] = json!({"audience":{"organization_id":"org","workspace_id":"workspace",
        "installation_id":"installation","generation":"1","product":"alt",
        "origin":"https://alt.example.test","control_origin":"https://cloud.example.test"},
        "runtime_credential_file":"runtime.key"});
    fs::write(root.join("config.json"), serde_json::to_vec(&c).unwrap()).unwrap();
    c
}
pub(super) fn channel() -> Value {
    let key = alt_crypto::ed25519::SigningKey::from_bytes(&[7; 32]);
    let public: String = key
        .verifying_key()
        .as_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    json!({"scope":{"installation_id":"installation","repository_instance_id":"repository","recovery_epoch":"1"},
        "channel":"channel","application":"application","peer":{"product":"sentori",
        "installation":"sentori","workspace":"workspace","scope":"project","generation":"1"},"peer_public_key":public,
        "target":{"origin":"http://127.0.0.1:8080","policy":{"kind":"pinned","addresses":["127.0.0.1"],"allow_plaintext":true}},
        "local_credential_file":"local.key","peer_credential_file":"peer.key"})
}
fn settings(c: &Value) -> Value {
    json!({"version":1,"instance":c["instance"],"channels":[channel()]})
}

#[test]
fn cloud_settings_survive_reads_without_changing_frozen_intent() {
    let root = alt_os::temp::tempdir().unwrap();
    let c = fixture(root.path());
    let path = root.path().join("config.json");
    let frozen = fs::read(&path).unwrap();
    assert!(Config::read(&path).unwrap().change_links.is_empty());
    create_private(
        &root.path().join("change-links.json"),
        &serde_json::to_vec(&settings(&c)).unwrap(),
    )
    .unwrap();
    for _ in 0..2 {
        let config = Config::read(&path).unwrap();
        assert_eq!(config.change_links.len(), 1);
        assert_eq!(
            config.change_links[0].local_credential_file,
            root.path().canonicalize().unwrap().join("local.key")
        );
        assert_eq!(fs::read(&path).unwrap(), frozen);
    }
}

#[test]
fn cloud_settings_reject_wrong_generation_scope_version_and_ambiguity() {
    let root = alt_os::temp::tempdir().unwrap();
    let c = fixture(root.path());
    let path = root.path().join("config.json");
    let sidecar = root.path().join("change-links.json");
    create_private(&sidecar, b"{}").unwrap();
    let original = settings(&c);
    for pointer in ["/instance/runtime_generation", "/version"] {
        let mut bad = original.clone();
        *bad.pointer_mut(pointer).unwrap() = json!(2);
        fs::write(&sidecar, serde_json::to_vec(&bad).unwrap()).unwrap();
        assert!(Config::read(&path).is_err(), "{pointer}");
    }
    let mut bad = original.clone();
    bad["channels"][0]["scope"]["installation_id"] = json!("other");
    fs::write(&sidecar, serde_json::to_vec(&bad).unwrap()).unwrap();
    assert!(Config::read(&path).is_err());
    fs::write(&sidecar, serde_json::to_vec(&original).unwrap()).unwrap();
    let mut inline = c;
    inline["change_links"] = json!([channel()]);
    fs::write(&path, serde_json::to_vec(&inline).unwrap()).unwrap();
    assert!(Config::read(&path).is_err());
}

#[test]
fn cloud_settings_fail_closed_for_unsafe_or_malformed_present_files() {
    let root = alt_os::temp::tempdir().unwrap();
    fixture(root.path());
    let path = root.path().join("config.json");
    let sidecar = root.path().join("change-links.json");
    for bytes in [b"{".to_vec(), vec![b' '; 65537]] {
        create_private(&sidecar, &bytes).unwrap();
        assert!(Config::read(&path).is_err());
        fs::remove_file(&sidecar).unwrap();
    }
    symlink(root.path().join("absent"), &sidecar).unwrap();
    assert!(Config::read(&path).is_err());
    fs::remove_file(&sidecar).unwrap();
    fs::create_dir(&sidecar).unwrap();
    assert!(Config::read(&path).is_err());
    fs::remove_dir(&sidecar).unwrap();
    create_private(&sidecar, b"{}").unwrap();
    fs::set_permissions(&sidecar, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(Config::read(&path).is_err());
}

#[test]
fn cloud_settings_keep_effective_configuration_within_backup_read_limit() {
    let root = alt_os::temp::tempdir().unwrap();
    let config = fixture(root.path());
    let path = root.path().join("config.json");
    let sidecar = root.path().join("change-links.json");
    let mut value = settings(&config);
    value["channels"] = json!(
        (0..9)
            .map(|i| {
                let mut c = channel();
                c["channel"] = json!(format!("channel-{i}"));
                c
            })
            .collect::<Vec<_>>()
    );
    // Valid component lengths and total paths below PATH_MAX; no file contents
    // are needed by the pure configuration reader. Both input files fit alone.
    let mut encoded = Vec::new();
    for length in 1000..3900 {
        let long = format!("/{}token", "segment/".repeat(length / 8));
        for c in value["channels"].as_array_mut().unwrap() {
            c["local_credential_file"] = json!(long);
            c["peer_credential_file"] = json!(long);
        }
        encoded = serde_json::to_vec(&value).unwrap();
        if encoded.len() >= 65000 {
            break;
        }
    }
    assert!((65000..=65536).contains(&encoded.len()));
    assert!(fs::read(&path).unwrap().len() < 65536);
    create_private(&sidecar, &encoded).unwrap();
    let result = Config::read(&path);
    assert!(matches!(
        result,
        Err(alt_server::operator::Error::Config(
            "effective configuration size"
        ))
    ));
}
