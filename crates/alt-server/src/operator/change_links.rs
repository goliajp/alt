//! Mutable operator pairing settings are separate from frozen Cloud intent.
use super::{Config, Error, Result, read_regular};
use alt_remote_protocol::DeploymentMode;
use alt_server_store::InstanceSpec;
use serde::Deserialize;
use std::{os::unix::fs::PermissionsExt, path::Path};

#[derive(Deserialize, alt_json::FromJson)]
#[serde(deny_unknown_fields)]
#[json(deny_unknown_fields)]
struct Settings {
    version: u32,
    instance: InstanceSpec,
    channels: Vec<crate::change_links::ChannelConfig>,
}

pub(super) fn load(config: &mut Config, directory: &Path) -> Result<()> {
    if config.instance.deployment_mode != DeploymentMode::Cloud {
        return Ok(());
    }
    let path = directory.join("change-links.json");
    match path.symlink_metadata() {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(Error::File),
        Ok(_) => {}
    }
    if directory
        .metadata()
        .map_err(|_| Error::File)?
        .permissions()
        .mode()
        & 0o022
        != 0
    {
        return Err(Error::Config("change-link directory permissions"));
    }
    if !config.change_links.is_empty() {
        return Err(Error::Config("ambiguous change-link configuration"));
    }
    let settings: Settings = alt_json::from_slice(&read_regular(&path, 65536, true)?)
        .map_err(|_| Error::Config("change-link settings JSON"))?;
    // A copied file from a previous recovery generation must fail closed.
    // Runtime credentials and database pairings are checked independently.
    if settings.version != 1 || settings.instance != config.instance {
        return Err(Error::Config("change-link settings instance or version"));
    }
    config.change_links = settings.channels;
    Ok(())
}

#[cfg(test)]
mod json_tests {
    use alt_json_testkit::arbitrary::from_serde;
    use alt_json_testkit::drive::exercise_reader;
    use alt_json_testkit::fixtures::{check_recorded, record_reader, verify_reader};
    use alt_json_testkit::rng::Rng;

    use super::*;

    const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/json");

    fn settings(rng: &mut Rng) -> alt_json::Value {
        let instance: InstanceSpec = from_serde(rng);
        let channel = alt_json::json!({
            "scope": {"installation_id": "installation", "repository_instance_id": "repository", "recovery_epoch": "1"},
            "channel": "channel",
            "application": "application",
            "peer": {"product": "sentori", "installation": "sentori", "workspace": "workspace", "scope": "project", "generation": "1"},
            "peer_public_key": "ab",
            "target": {"origin": "http://127.0.0.1:8080", "policy": {"kind": "pinned", "addresses": ["127.0.0.1"], "allow_plaintext": true}},
            "local_credential_file": "local.key",
            "peer_credential_file": "peer.key",
        });
        let channels = vec![channel; rng.below(3) as usize];
        alt_json::json!({"version": rng.below(3), "instance": instance, "channels": channels})
    }

    fn view(value: &Settings) -> String {
        format!(
            "{} {} {}",
            value.version,
            alt_json::to_string(&value.instance).unwrap(),
            alt_json::to_string(&value.channels).unwrap()
        )
    }

    #[test]
    fn same_as_serde() {
        exercise_reader::<Settings>(0xd400, 200, settings, view).report("change-link settings");
    }

    #[test]
    fn recorded_verdicts() {
        check_recorded(
            &format!("{DIR}/operator-change_links-Settings.txt"),
            &record_reader::<Settings>(0xd401, 10, settings),
        );
        verify_reader::<Settings>(DIR, "operator-change_links-Settings");
    }
}
