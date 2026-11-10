//! Explicit instance configuration and local operator operations. Runtime open
//! never initializes, migrates, or bootstraps an identity on configuration loss.
use crate::{HttpOptions, NativeOptions, StoreFactory};
use alt_remote_protocol::Limits;
use alt_server_store::{ByteStore, InstanceSpec, Store};
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    net::SocketAddr,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::Duration,
};

pub mod backup;
mod change_links;
pub mod provision;

#[derive(Debug)]
pub enum Error {
    Config(&'static str),
    File,
    Database,
    Store,
    Unknown,
    RestoreStage(&'static str),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Config(v) => write!(f, "invalid operator configuration: {v}"),
            Self::File => f.write_str("operator file is unavailable or could not be persisted"),
            Self::Database => f.write_str("database connection is unavailable"),
            Self::Store => f.write_str("instance is not ready or operator action was refused"),
            Self::Unknown => f.write_str(
                "operator action outcome is unknown; inspect the same instance before retrying",
            ),
            Self::RestoreStage(v) => write!(
                f,
                "restoration incomplete at {v}; inspect the isolated destination before retrying"
            ),
        }
    }
}

impl std::error::Error for Error {}
impl From<alt_server_store::Error> for Error {
    fn from(error: alt_server_store::Error) -> Self {
        match error {
            alt_server_store::Error::OutcomeUnknown => Self::Unknown,
            _ => Self::Store,
        }
    }
}
pub type Result<T> = std::result::Result<T, Error>;
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub change_links: Vec<crate::change_links::ChannelConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub portable: Option<alt_portable::http::Bounds>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloud: Option<CloudConfig>,
    pub version: u32,
    pub instance: InstanceSpec,
    /// Name of an explicit environment variable, never the DB secret itself.
    pub database_environment: String,
    pub bytes_directory: PathBuf,
    pub receipt_key_file: PathBuf,
    /// The supplied listener is a private loopback origin behind the packaged
    /// HTTPS edge. Public plaintext listeners are deliberately unavailable.
    pub listen: SocketAddr,
    pub limits: Limits,
    pub tasks: alt_task_service::TaskLimits,
    pub http: HttpBounds,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser: Option<BrowserConfig>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloudConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provision_digest: Option<String>,
    pub audience: golia_platform_contract::Audience,
    pub runtime_credential_file: PathBuf,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserConfig {
    pub public_origin: String,
    pub assets_directory: PathBuf,
    pub session_ttl_ms: u64,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpBounds {
    pub connections: usize,
    pub workers: usize,
    pub header_bytes: usize,
    pub headers: usize,
    pub header_timeout_ms: u64,
    pub body_timeout_ms: u64,
    pub work_timeout_ms: u64,
    pub connection_timeout_ms: u64,
    pub database_connect_timeout_ms: u64,
    pub database_statement_timeout_ms: u64,
    pub upload_ttl_ms: u64,
    pub worker_lease_ms: u64,
    pub approval_ttl_ms: u64,
}
impl Config {
    /// Accept either an explicitly issued raw operator secret or the private,
    /// origin-bound envelope produced by `alt auth login`.
    pub fn read_credential(&self, path: &Path) -> Result<String> {
        let bytes = read_regular(path, 16_384, true)?;
        if bytes.first() != Some(&b'{') {
            return read_token(path);
        }
        let value: alt_remote_protocol::login::SavedCredential = serde_json::from_slice(&bytes)
            .map_err(|_| Error::Config("credential file encoding"))?;
        value
            .validate()
            .map_err(|_| Error::Config("credential file fields"))?;
        let key: String = self
            .signing_key()?
            .verifying_key()
            .as_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        if self.browser.as_ref().map(|b| b.public_origin.as_str()) != Some(value.origin.as_str())
            || self.instance.installation_id != value.installation_id
            || value.receipt_public_key != key
        {
            return Err(Error::Config("credential belongs to a different instance"));
        }
        Ok(value.access_token)
    }
    pub fn read(path: &Path) -> Result<Self> {
        let bytes = read_regular(path, 65536, false)?;
        let mut config: Self =
            serde_json::from_slice(&bytes).map_err(|_| Error::Config("configuration JSON"))?;
        let parent = std::fs::canonicalize(
            path.parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new(".")),
        )
        .map_err(|_| Error::File)?;
        change_links::load(&mut config, &parent)?;
        for item in [&mut config.bytes_directory, &mut config.receipt_key_file] {
            if item.is_relative() {
                *item = parent.join(&*item);
            }
        }
        if let Some(browser) = &mut config.browser
            && browser.assets_directory.is_relative()
        {
            browser.assets_directory = parent.join(&browser.assets_directory);
        }
        if let Some(cloud) = &mut config.cloud
            && cloud.runtime_credential_file.is_relative()
        {
            cloud.runtime_credential_file = parent.join(&cloud.runtime_credential_file);
        }
        for channel in &mut config.change_links {
            for path in [
                &mut channel.local_credential_file,
                &mut channel.peer_credential_file,
            ] {
                if path.is_relative() {
                    *path = parent.join(&*path);
                }
            }
        }
        config.validate()?;
        // Backups serialize the effective configuration, including separately
        // loaded Cloud settings and resolved paths, into a bounded archive entry.
        if serde_json::to_vec(&config)
            .map_err(|_| Error::Config("configuration encoding"))?
            .len()
            > 65536
        {
            return Err(Error::Config("effective configuration size"));
        }
        Ok(config)
    }
    pub fn validate(&self) -> Result<()> {
        use alt_remote_protocol::DeploymentMode;
        let mut identifiers = std::collections::BTreeSet::new();
        if self.change_links.len() > 32 {
            return Err(Error::Config("change-link configuration limit"));
        }
        for channel in &self.change_links {
            channel.validate(&self.instance)?;
            if !identifiers.insert((
                &channel.scope.repository_instance_id,
                channel.scope.recovery_epoch,
                &channel.channel,
            )) {
                return Err(Error::Config("duplicate change-link configuration"));
            }
        }
        match (&self.cloud, self.instance.deployment_mode) {
            (Some(cloud), DeploymentMode::Cloud) => {
                cloud
                    .audience
                    .validate()
                    .map_err(|_| Error::Config("Cloud audience"))?;
                if cloud
                    .provision_digest
                    .as_deref()
                    .is_some_and(|d| !golia_platform_contract::lifecycle::lower_hex_32(d))
                {
                    return Err(Error::Config("Cloud provision digest"));
                }
                let a = &cloud.audience;
                if a.product != golia_platform_contract::Product::Alt
                    || a.installation_id.as_str() != self.instance.installation_id
                    || a.workspace_id.as_str() != self.instance.workspace_id
                    || a.generation.0 != self.instance.runtime_generation
                    || !cloud.runtime_credential_file.is_absolute()
                    || self.browser.as_ref().is_some_and(|b| {
                        b.public_origin != a.origin.as_str() || b.session_ttl_ms > 8 * 3_600_000
                    })
                {
                    return Err(Error::Config("Cloud instance binding"));
                }
            }
            (None, DeploymentMode::SelfHosted) => (),
            _ => {
                return Err(Error::Config(
                    "deployment mode requires exact Cloud configuration",
                ));
            }
        }
        if let Some(browser) = &self.browser {
            alt_browser_auth::PublicOrigin::parse(&browser.public_origin)
                .map_err(|_| Error::Config("browser public origin"))?;
            if !browser.assets_directory.is_absolute()
                || !(60_000..=43_200_000).contains(&browser.session_ttl_ms)
            {
                return Err(Error::Config("browser assets/session limits"));
            }
        }
        self.limits
            .validate()
            .map_err(|_| Error::Config("transfer limits"))?;
        alt_remote_protocol::Scope {
            installation_id: self.instance.installation_id.clone(),
            repository_instance_id: self.instance.workspace_id.clone(),
            recovery_epoch: self.instance.runtime_generation,
        }
        .validate()
        .map_err(|_| Error::Config("instance identity"))?;
        if self.version != 1
            || !self.listen.ip().is_loopback()
            || !self.bytes_directory.is_absolute()
            || !self.receipt_key_file.is_absolute()
            || self.database_environment.is_empty()
            || self.database_environment.len() > 128
            || !self
                .database_environment
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        {
            return Err(Error::Config(
                "version, private listener, paths or explicit database environment",
            ));
        }
        let h = &self.http;
        if h.connections == 0
            || h.connections > 65536
            || h.workers == 0
            || h.workers > h.connections
            || h.header_bytes < 8192
            || h.header_bytes > 1024 * 1024
            || h.headers == 0
            || h.headers > 1024
            || [
                h.header_timeout_ms,
                h.body_timeout_ms,
                h.work_timeout_ms,
                h.connection_timeout_ms,
                h.database_connect_timeout_ms,
                h.database_statement_timeout_ms,
                h.upload_ttl_ms,
                h.worker_lease_ms,
                h.approval_ttl_ms,
            ]
            .into_iter()
            .any(|n| n == 0 || n > 86_400_000)
            || h.database_statement_timeout_ms > 300_000
            || h.database_connect_timeout_ms > 300_000
            || h.worker_lease_ms < h.work_timeout_ms
        {
            return Err(Error::Config("HTTP/worker bounds"));
        }
        if let Some(portable) = self.portable {
            portable
                .verification(self.limits, self.tasks)
                .map_err(|_| Error::Config("portable validation bounds"))?;
        }
        let t = &self.tasks;
        if [
            t.objects,
            t.records,
            t.edges,
            t.structured_bytes,
            t.paths,
            t.path_bytes,
            t.walk_steps,
        ]
        .into_iter()
        .any(|n| n == 0 || n > u32::MAX as usize)
            || t.artifact_bytes == 0
            || t.total_bytes < t.artifact_bytes
            || t.work_bytes < t.total_bytes
        {
            return Err(Error::Config("task validation bounds"));
        }
        Ok(())
    }
    pub fn database(&self) -> Result<postgres::Config> {
        self.validate()?;
        let value = std::env::var(&self.database_environment)
            .map_err(|_| Error::Config("required database environment missing"))?;
        let mut config: postgres::Config = value
            .parse()
            .map_err(|_| Error::Config("database connection parameters"))?;
        // The v1 POSIX adapter uses a local DB socket/sidecar. A remote DB needs
        // an explicit TLS adapter, never an implicit plaintext downgrade.
        if config.get_hosts().is_empty()
            || config.get_hosts().iter().any(|host| match host {
                postgres::config::Host::Tcp(host) => !host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback()),
                postgres::config::Host::Unix(path) => !path.is_absolute(),
            })
            || config.get_hostaddrs().iter().any(|ip| !ip.is_loopback())
        {
            return Err(Error::Config(
                "database must use an explicit local socket or literal loopback address",
            ));
        }
        config.connect_timeout(Duration::from_millis(self.http.database_connect_timeout_ms));
        Ok(config)
    }
    pub fn client(&self) -> Result<postgres::Client> {
        let mut client = self
            .database()?
            .connect(postgres::NoTls)
            .map_err(|_| Error::Database)?;
        let timeout = self.http.database_statement_timeout_ms;
        client
            .batch_execute(&format!(
                "SET statement_timeout={timeout}; SET lock_timeout={timeout}"
            ))
            .map_err(|_| Error::Database)?;
        Ok(client)
    }
    pub fn signing_key(&self) -> Result<SigningKey> {
        let bytes: [u8; 32] = read_regular(&self.receipt_key_file, 32, true)?
            .try_into()
            .map_err(|_| Error::Config("receipt key must contain 32 raw bytes"))?;
        Ok(SigningKey::from_bytes(&bytes))
    }
    pub fn cloud_runtime(&self) -> Result<Option<std::sync::Arc<crate::cloud::Runtime>>> {
        self.validate()?;
        self.cloud
            .as_ref()
            .map(|cloud| {
                let bytes = read_regular(&cloud.runtime_credential_file, 65, true)?;
                let text = std::str::from_utf8(&bytes)
                    .map_err(|_| Error::Config("Cloud runtime credential"))?;
                let credential = text
                    .strip_suffix('\n')
                    .unwrap_or(text)
                    .to_owned()
                    .try_into()
                    .map_err(|_| Error::Config("Cloud runtime credential"))?;
                crate::cloud::Runtime::new(cloud.audience.clone(), credential)
                    .and_then(|runtime| match &cloud.provision_digest {
                        Some(digest) => runtime.provisioned(digest.clone()),
                        None => Ok(runtime),
                    })
                    .map(std::sync::Arc::new)
                    .map_err(|_| Error::Config("Cloud runtime credential"))
            })
            .transpose()
    }
    pub fn open(&self) -> Result<Store> {
        Ok(self.factory()?.open()?)
    }
    fn volume_identity(&self) -> Result<serde_json::Value> {
        Ok(
            serde_json::json!({"version":1,"installation_id":self.instance.installation_id,"workspace_id":self.instance.workspace_id,"deployment_mode":self.instance.deployment_mode,"receipt_key_id":alt_hash::blake3::hash(self.signing_key()?.verifying_key().as_bytes()).to_hex().to_string()}),
        )
    }
    pub(super) fn check_volume(&self) -> Result<()> {
        let marker: serde_json::Value = serde_json::from_slice(&read_regular(
            &self.bytes_directory.join("instance.json"),
            4096,
            true,
        )?)
        .map_err(|_| Error::Config("volume identity"))?;
        if marker != self.volume_identity()? {
            return Err(Error::Config(
                "volume belongs to another installation or receipt key",
            ));
        }
        Ok(())
    }
    pub fn initialize(&self, resume: bool) -> Result<Store> {
        self.validate()?;
        let cloud = self.cloud_runtime()?;
        let signing_key = self.signing_key()?;
        let bytes = ByteStore::open(&self.bytes_directory)?;
        let marker = self.bytes_directory.join("instance.json");
        if marker.exists() {
            if !resume {
                return Err(Error::Config(
                    "volume already initialized; explicit resume required",
                ));
            }
            self.check_volume()?;
        } else {
            if std::fs::read_dir(&self.bytes_directory)
                .map_err(|_| Error::File)?
                .next()
                .is_some()
            {
                return Err(Error::Config("new volume must be empty"));
            }
            create_private(
                &marker,
                &serde_json::to_vec(&self.volume_identity()?)
                    .map_err(|_| Error::Config("volume identity"))?,
            )?;
        }
        let mut client = self.client()?;
        let exists: bool = client
            .query_one("SELECT to_regclass('public.alt_instance') IS NOT NULL", &[])
            .map_err(|_| Error::Database)?
            .get(0);
        if exists {
            if !resume {
                return Err(Error::Config(
                    "database already initialized; explicit resume required",
                ));
            }
            let mut store = Store::open(
                client,
                bytes,
                self.instance.clone(),
                signing_key,
                self.limits,
            )?;
            if let Some(cloud) = &cloud {
                cloud.check(&mut store)?;
            }
            Ok(store)
        } else if let Some(cloud) = cloud {
            if let Some(digest) = cloud.provision_digest() {
                return Ok(Store::initialize_cloud_provisioned(
                    client,
                    bytes,
                    self.instance.clone(),
                    signing_key,
                    self.limits,
                    cloud.audience(),
                    cloud.key_digest(),
                    digest,
                )?);
            }
            Ok(Store::initialize_cloud(
                client,
                bytes,
                self.instance.clone(),
                signing_key,
                self.limits,
                cloud.audience(),
                cloud.key_digest(),
            )?)
        } else {
            Ok(Store::initialize(
                client,
                bytes,
                self.instance.clone(),
                signing_key,
                self.limits,
            )?)
        }
    }
    pub fn factory(&self) -> Result<StoreFactory> {
        self.check_volume()?;
        Ok(StoreFactory {
            change_links: self
                .change_links
                .iter()
                .map(|c| c.load(self))
                .collect::<Result<Vec<_>>>()?
                .into(),
            cloud: self.cloud_runtime()?,
            database: self.database()?,
            bytes: ByteStore::open(&self.bytes_directory)?,
            instance: self.instance.clone(),
            signing_key: self.signing_key()?,
            limits: self.limits,
            connect_timeout: Duration::from_millis(self.http.database_connect_timeout_ms),
            statement_timeout: Duration::from_millis(self.http.database_statement_timeout_ms),
        })
    }
    /// Save the next configuration before the database commit. After a crash,
    /// the operator can query it directly; re-running this same operation does
    /// not silently choose a second generation or overwrite another config.
    pub fn migrate(&self, from: u32, output: &Path) -> Result<Self> {
        if self.cloud.is_some() {
            return Err(Error::Config(
                "Cloud migration requires coordinated platform generation binding",
            ));
        }
        self.check_volume()?;
        if !(1..alt_server_store::SCHEMA_VERSION).contains(&from) {
            return Err(Error::Config("unsupported source schema"));
        }
        let mut next = self.clone();
        next.instance.runtime_generation = next
            .instance
            .runtime_generation
            .checked_add(1)
            .ok_or(Error::Config("runtime generation overflow"))?;
        next.validate()?;
        let encoded =
            serde_json::to_vec_pretty(&next).map_err(|_| Error::Config("next configuration"))?;
        if output.exists() {
            if read_regular(output, 65536, true)? != encoded {
                return Err(Error::Config(
                    "next configuration already exists with different contents",
                ));
            }
            if next.open().is_ok() {
                return Ok(next);
            }
        } else {
            create_private(output, &encoded)?;
        }
        let client = self.client()?;
        let bytes = ByteStore::open(&self.bytes_directory)?;
        let key = self.signing_key()?;
        let migrated = match from {
            1 => Store::migrate_v1(client, bytes, self.instance.clone(), key, self.limits),
            2 => Store::migrate_v2(client, bytes, self.instance.clone(), key, self.limits),
            3 => Store::migrate_v3(client, bytes, self.instance.clone(), key, self.limits),
            4 => Store::migrate_v4(client, bytes, self.instance.clone(), key, self.limits),
            5 => Store::migrate_v5(client, bytes, self.instance.clone(), key, self.limits),
            7 => Store::migrate_v7(client, bytes, self.instance.clone(), key, self.limits),
            8 => Store::migrate_v8(client, bytes, self.instance.clone(), key, self.limits),
            9 => Store::migrate_v9(client, bytes, self.instance.clone(), key, self.limits),
            13 => Store::migrate_v13(client, bytes, self.instance.clone(), key, self.limits),
            14 => Store::migrate_v14(client, bytes, self.instance.clone(), key, self.limits),
            15 => Store::migrate_v15(client, bytes, self.instance.clone(), key, self.limits),
            16 => Store::migrate_v16(client, bytes, self.instance.clone(), key, self.limits),
            12 => Store::migrate_v12(client, bytes, self.instance.clone(), key, self.limits),
            11 => Store::migrate_v11(client, bytes, self.instance.clone(), key, self.limits),
            10 => Store::migrate_v10(client, bytes, self.instance.clone(), key, self.limits),
            6 => Store::migrate_v6(client, bytes, self.instance.clone(), key, self.limits),
            _ => unreachable!(),
        };
        match migrated {
            Ok(store) if store.spec() == &next.instance => Ok(next),
            Err(alt_server_store::Error::OutcomeUnknown) if next.open().is_ok() => Ok(next),
            Err(error) => Err(error.into()),
            _ => Err(Error::Unknown),
        }
    }
    pub fn browser_options(&self) -> Result<Option<crate::WebOptions>> {
        self.browser
            .as_ref()
            .map(|browser| {
                Ok(crate::WebOptions {
                    origin: alt_browser_auth::PublicOrigin::parse(&browser.public_origin)
                        .map_err(|_| Error::Config("browser origin"))?,
                    assets: crate::WebAssets::load(&browser.assets_directory)
                        .map_err(|_| Error::Config("workbench assets"))?,
                    session_ttl_ms: browser.session_ttl_ms,
                })
            })
            .transpose()
    }
    pub fn http_options(&self) -> HttpOptions {
        let h = &self.http;
        HttpOptions {
            connections: h.connections,
            workers: h.workers,
            header_bytes: h.header_bytes,
            headers: h.headers,
            header_timeout: Duration::from_millis(h.header_timeout_ms),
            body_timeout: Duration::from_millis(h.body_timeout_ms),
            work_timeout: Duration::from_millis(h.work_timeout_ms),
            connection_timeout: Duration::from_millis(h.connection_timeout_ms),
            native: NativeOptions {
                portable: self.portable,
                task_limits: self.tasks,
                upload_ttl_ms: h.upload_ttl_ms,
                worker_lease_ms: h.worker_lease_ms,
                approval_ttl_ms: h.approval_ttl_ms,
            },
        }
    }
}
pub fn read_regular(path: &Path, limit: usize, private: bool) -> Result<Vec<u8>> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| Error::File)?;
    let meta = file.metadata().map_err(|_| Error::File)?;
    if !meta.is_file()
        || meta.len() > limit as u64
        || (private && meta.permissions().mode() & 0o077 != 0)
    {
        return Err(Error::Config(
            "operator file type, size or private permissions",
        ));
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::File)?;
    if bytes.len() > limit {
        return Err(Error::Config("operator file size"));
    }
    Ok(bytes)
}
pub fn read_token(path: &Path) -> Result<String> {
    let value = String::from_utf8(read_regular(path, 70, true)?)
        .map_err(|_| Error::Config("token file encoding"))?;
    let token = value.strip_suffix('\n').unwrap_or(&value);
    if token.len() != 69
        || !token.starts_with("alt1_")
        || !token.as_bytes()[5..]
            .iter()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
    {
        return Err(Error::Config("token file format"));
    }
    Ok(token.into())
}
/// Install secrets only into an explicit, previously absent file, fsyncing both
/// file and parent before any database registration can occur.
pub fn create_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(|_| Error::File)?;
    temporary
        .as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o600))
        .map_err(|_| Error::File)?;
    temporary.write_all(bytes).map_err(|_| Error::File)?;
    temporary.as_file().sync_all().map_err(|_| Error::File)?;
    temporary.persist_noclobber(path).map_err(|_| Error::File)?;
    File::open(parent)
        .and_then(|f| f.sync_all())
        .map_err(|_| Error::Unknown)
}
