//! Explicit Cloud initialization from a frozen regional specification. Every
//! local artifact is durable before the database transaction; retries compare
//! the original intent and never generate another identity or key.
use super::*;
use golia_platform_contract::{Product, lifecycle::ProvisionSpec};

fn ensure(path: &Path, bytes: &[u8]) -> Result<()> {
    if path.symlink_metadata().is_ok() {
        if read_regular(path, bytes.len(), true)? != bytes {
            return Err(Error::Config(
                "provision artifact differs from persisted intent",
            ));
        }
        Ok(())
    } else {
        create_private(path, bytes)
    }
}

pub fn initialize(
    spec_path: &Path,
    directory: &Path,
    assets: &Path,
    listen: SocketAddr,
    database_environment: &str,
) -> Result<Config> {
    let spec: ProvisionSpec = serde_json::from_slice(&read_regular(spec_path, 16384, true)?)
        .map_err(|_| Error::Config("provision specification"))?;
    spec.validate()
        .map_err(|_| Error::Config("provision specification"))?;
    if spec.audience.product != Product::Alt {
        return Err(Error::Config("initial Alt generation required"));
    }
    let meta = directory.symlink_metadata().map_err(|_| Error::File)?;
    if !meta.is_dir() || meta.permissions().mode() & 0o077 != 0 {
        return Err(Error::Config(
            "private existing provision directory required",
        ));
    }
    let directory = directory.canonicalize().map_err(|_| Error::File)?;
    if spec.audience.generation.0 != 1 {
        // A restored generation may only open a completed recovery; it must
        // never take the initialization path on an empty or partial database.
        let recovery: golia_platform_contract::lifecycle::RecoverySpec = serde_json::from_slice(
            &read_regular(&directory.join("recovery.json"), 65536, true)?,
        )
        .map_err(|_| Error::Config("recovery intent"))?;
        recovery
            .validate()
            .map_err(|_| Error::Config("recovery intent"))?;
        if recovery.target.digest().map_err(|_| Error::File)?
            != spec.digest().map_err(|_| Error::File)?
        {
            return Err(Error::Config("recovery target changed"));
        }
        let config = configuration(&spec, &directory, assets, listen, database_environment)?;
        verify_artifacts(&config, &spec, &directory)?;
        config
            .open()?
            .check_restored_backup(&recovery.backup.manifest_digest)?;
        return Ok(config);
    }
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(alt_os::sys::O_NOFOLLOW | alt_os::sys::O_CLOEXEC)
        .open(directory.join("initialize.lock"))
        .map_err(|_| Error::File)?;
    lock.try_lock()
        .map_err(|_| Error::Config("initialization already running"))?;
    let intent = directory.join("provision.json");
    if intent.symlink_metadata().is_err() {
        for entry in std::fs::read_dir(&directory).map_err(|_| Error::File)? {
            if entry.map_err(|_| Error::File)?.file_name() != "initialize.lock" {
                return Err(Error::Config("initial provision directory must be empty"));
            }
        }
    }
    let config = configuration(&spec, &directory, assets, listen, database_environment)?;
    ensure(
        &intent,
        &serde_json::to_vec(&spec).map_err(|_| Error::Config("provision encoding"))?,
    )?;
    let mut signing = [0u8; 32];
    for (index, byte) in signing.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&spec.data_key.expose()[index * 2..index * 2 + 2], 16)
            .map_err(|_| Error::Config("provision key"))?;
    }
    ensure(&config.receipt_key_file, &signing)?;
    ensure(
        &directory.join("runtime.key"),
        spec.runtime_key.expose().as_bytes(),
    )?;
    ensure(
        &directory.join("config.json"),
        &serde_json::to_vec_pretty(&config)
            .map_err(|_| Error::Config("provision configuration"))?,
    )?;
    config.initialize(true)?;
    Ok(config)
}

fn configuration(
    spec: &ProvisionSpec,
    directory: &Path,
    assets: &Path,
    listen: SocketAddr,
    database_environment: &str,
) -> Result<Config> {
    let mut config: Config = serde_json::from_str(include_str!("../../example-config.json"))
        .map_err(|_| Error::Config("packaged Cloud defaults"))?;
    // Older frozen provisioning intents predate portable transfer. Restarting
    // one must preserve its opt-out and exact config bytes. Other saved fields
    // still have to match the specification through ensure/verify_artifacts.
    let saved = directory.join("config.json");
    if saved.symlink_metadata().is_ok() {
        let prior: Config = serde_json::from_slice(&read_regular(&saved, 65536, true)?)
            .map_err(|_| Error::Config("saved provision configuration"))?;
        if prior.portable.is_none() {
            config.portable = None;
        }
    }
    config.instance = InstanceSpec {
        installation_id: spec.audience.installation_id.as_str().into(),
        workspace_id: spec.audience.workspace_id.as_str().into(),
        runtime_generation: spec.audience.generation.0,
        deployment_mode: alt_remote_protocol::DeploymentMode::Cloud,
    };
    config.cloud = Some(CloudConfig {
        audience: spec.audience.clone(),
        runtime_credential_file: directory.join("runtime.key"),
        provision_digest: Some(
            spec.digest()
                .map_err(|_| Error::Config("provision digest"))?,
        ),
    });
    config.browser = Some(BrowserConfig {
        public_origin: spec.audience.origin.as_str().into(),
        assets_directory: assets.canonicalize().map_err(|_| Error::File)?,
        session_ttl_ms: 8 * 3_600_000,
    });
    config.listen = listen;
    config.database_environment = database_environment.into();
    config.bytes_directory = directory.join("bytes");
    config.receipt_key_file = directory.join("receipt.key");
    config.validate()?;
    // Validate the explicit local DSN before writing intent; no connection yet.
    config.database()?;
    Ok(config)
}

fn verify_artifacts(config: &Config, spec: &ProvisionSpec, directory: &Path) -> Result<()> {
    let saved = read_regular(&directory.join("config.json"), 65536, true)?;
    let key = read_regular(&directory.join("runtime.key"), 64, true)?;
    if saved != serde_json::to_vec_pretty(config).map_err(|_| Error::File)?
        || key != spec.runtime_key.expose().as_bytes()
    {
        return Err(Error::Config("recovery configuration changed"));
    }
    Ok(())
}

/// Prepare only the immutable destination intent. The existing backup operator
/// owns archive verification, restoration, generation transition and its receipt.
pub fn prepare_recovery(
    spec: &golia_platform_contract::lifecycle::RecoverySpec,
    directory: &Path,
    assets: &Path,
    listen: SocketAddr,
    database_environment: &str,
) -> Result<Config> {
    spec.validate()
        .map_err(|_| Error::Config("recovery specification"))?;
    let meta = directory.symlink_metadata().map_err(|_| Error::File)?;
    if !meta.is_dir() || meta.permissions().mode() & 0o077 != 0 {
        return Err(Error::Config("private recovery directory required"));
    }
    let directory = directory.canonicalize().map_err(|_| Error::File)?;
    let intent = directory.join("recovery.json");
    if intent.symlink_metadata().is_err()
        && std::fs::read_dir(&directory)
            .map_err(|_| Error::File)?
            .next()
            .is_some()
    {
        return Err(Error::Config("initial recovery directory must be empty"));
    }
    let config = configuration(
        &spec.target,
        &directory,
        assets,
        listen,
        database_environment,
    )?;
    ensure(&intent, &serde_json::to_vec(spec).map_err(|_| Error::File)?)?;
    ensure(
        &directory.join("provision.json"),
        &serde_json::to_vec(&spec.target).map_err(|_| Error::File)?,
    )?;
    ensure(
        &directory.join("runtime.key"),
        spec.target.runtime_key.expose().as_bytes(),
    )?;
    ensure(
        &directory.join("config.json"),
        &serde_json::to_vec_pretty(&config).map_err(|_| Error::File)?,
    )?;
    Ok(config)
}
