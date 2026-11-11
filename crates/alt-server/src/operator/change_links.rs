//! Mutable operator pairing settings are separate from frozen Cloud intent.
use super::{Config, Error, Result, read_regular};
use alt_remote_protocol::DeploymentMode;
use alt_server_store::InstanceSpec;
use std::{os::unix::fs::PermissionsExt, path::Path};

#[derive(alt_json::FromJson)]
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
    use super::*;

    #[test]
    fn recorded_verdicts() {
        alt_json_testkit::fixtures::verify_reader::<Settings>(
            concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/json"),
            "operator-change_links-Settings",
        );
    }
}
