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

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid operator configuration: {0}")]
    Config(&'static str),
    #[error("operator file is unavailable or could not be persisted")]
    File,
    #[error("database connection is unavailable")]
    Database,
    #[error("instance is not ready or operator action was refused")]
    Store,
    #[error("operator action outcome is unknown; inspect the same instance before retrying")]
    Unknown,
    #[error("restoration incomplete at {0}; inspect the isolated destination before retrying")]
    RestoreStage(&'static str),
}
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
        config.validate()?;
        Ok(config)
    }
    pub fn validate(&self) -> Result<()> {
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
    pub fn open(&self) -> Result<Store> {
        self.check_volume()?;
        Ok(Store::open(
            self.client()?,
            ByteStore::open(&self.bytes_directory)?,
            self.instance.clone(),
            self.signing_key()?,
            self.limits,
        )?)
    }
    fn volume_identity(&self) -> Result<serde_json::Value> {
        Ok(
            serde_json::json!({"version":1,"installation_id":self.instance.installation_id,"workspace_id":self.instance.workspace_id,"deployment_mode":self.instance.deployment_mode,"receipt_key_id":blake3::hash(self.signing_key()?.verifying_key().as_bytes()).to_hex().to_string()}),
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
            Ok(Store::open(
                client,
                bytes,
                self.instance.clone(),
                signing_key,
                self.limits,
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
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
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
