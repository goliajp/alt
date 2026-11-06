//! Encrypted, signed physical backups. The SQL archive and immutable bytes share
//! one fenced database snapshot. A completed manifest is published only after
//! the encrypted payload has been synced. Verification is not a restore drill.
use super::{Config, Error, Result, create_private, read_regular};
use alt_server_store::{ByteStore, MaintenanceSnapshot, SnapshotCatalog};
use base64::{Engine, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signature, Signer, VerifyingKey};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::{Read, Seek, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::Path,
    process::{Child, Command, Stdio},
};

const MANIFEST_LIMIT: usize = 64 * 1024 * 1024;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileDigest {
    pub bytes: u64,
    pub blake3: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub format: u32,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    pub catalog: SnapshotCatalog,
    pub files: BTreeMap<String, FileDigest>,
    pub payload: FileDigest,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SignedManifest {
    manifest: Manifest,
    signature: String,
}
struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn checked(process: &mut Process) -> Result<()> {
    if !process.0.wait().map_err(|_| Error::File)?.success() {
        return Err(Error::Config(
            "backup tool failed; no completed backup or restoration",
        ));
    }
    Ok(())
}
fn regular(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| Error::File)?;
    if !file.metadata().map_err(|_| Error::File)?.is_file() {
        return Err(Error::Config("backup entry is not a regular file"));
    }
    Ok(file)
}
fn digest(file: &mut File) -> Result<FileDigest> {
    file.rewind().map_err(|_| Error::File)?;
    let mut hash = blake3::Hasher::new();
    let mut bytes = 0u64;
    let mut buffer = [0u8; 65536];
    loop {
        let n = file.read(&mut buffer).map_err(|_| Error::File)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
        bytes = bytes
            .checked_add(n as u64)
            .ok_or(Error::Config("backup size overflow"))?;
    }
    file.rewind().map_err(|_| Error::File)?;
    Ok(FileDigest {
        bytes,
        blake3: hash.finalize().to_hex().to_string(),
    })
}
fn postgres_command(config: &Config, program: &str) -> Result<Command> {
    let db = config.database()?;
    let mut command = Command::new(program);
    // No DSN/password in argv or diagnostic output. Discard ambient PG service
    // configuration so the operator cannot accidentally dump another database.
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default());
    let host = match db.get_hosts() {
        [postgres::config::Host::Tcp(host)] => host.clone(),
        [postgres::config::Host::Unix(path)] => path
            .to_str()
            .ok_or(Error::Config("database socket encoding"))?
            .into(),
        _ => return Err(Error::Config("backup requires one explicit database host")),
    };
    if db.get_ports().len() > 1 || !db.get_hostaddrs().is_empty() {
        return Err(Error::Config("backup requires one database endpoint"));
    }
    command
        .env("PGHOST", host)
        .env(
            "PGPORT",
            db.get_ports().first().copied().unwrap_or(5432).to_string(),
        )
        .env(
            "PGDATABASE",
            db.get_dbname()
                .ok_or(Error::Config("explicit database name required"))?,
        )
        .env(
            "PGUSER",
            db.get_user()
                .ok_or(Error::Config("explicit database user required"))?,
        )
        .env(
            "PGCONNECT_TIMEOUT",
            (config.http.database_connect_timeout_ms / 1000)
                .max(1)
                .to_string(),
        )
        .env("PGSSLMODE", "disable")
        .arg("--no-password")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(password) = db.get_password() {
        use std::os::unix::ffi::OsStrExt;
        command.env("PGPASSWORD", std::ffi::OsStr::from_bytes(password));
    }
    Ok(command)
}
fn append<W: Write>(
    archive: &mut tar::Builder<W>,
    name: &str,
    file: &mut File,
) -> Result<FileDigest> {
    let description = digest(file)?;
    let mut header = tar::Header::new_gnu();
    header.set_size(description.bytes);
    header.set_mode(0o600);
    header.set_mtime(0);
    header.set_cksum();
    archive
        .append_data(&mut header, name, file)
        .map_err(|_| Error::File)?;
    Ok(description)
}
fn blob_path(blob: &alt_server_store::SnapshotBlob) -> String {
    format!("bytes/{}/{}", blob.repository, blob.blake3)
}
/// Physical access to the configuration, receipt key and database is required;
/// an HTTP administrator token alone cannot export passwords or signing keys.
/// The recipient is a public age key. Keep its private identity off the server.
pub fn create(config: &Config, schema: u32, recipient: &str, output: &Path) -> Result<Manifest> {
    // Historical schemas cannot record maintenance events. Their signed
    // completion manifest remains the authority; never migrate just to back up.
    let mut store = if schema == alt_server_store::SCHEMA_VERSION {
        Some(config.open()?)
    } else {
        None
    };
    let operation = alt_browser_auth::secret();
    if let Some(store) = &mut store {
        store.record_backup_event(&operation, alt_server_store::BackupEvent::Started)?;
    }
    let result = create_inner(config, schema, recipient, output, &operation);
    if let Some(store) = &mut store {
        let event = if result.is_ok() {
            alt_server_store::BackupEvent::Created
        } else {
            // This does not claim rollback. Inspect the manifest at the original
            // output location; a crash can leave only the start event.
            alt_server_store::BackupEvent::Unconfirmed
        };
        store
            .record_backup_event(&operation, event)
            .map_err(|_| Error::Unknown)?;
    }
    result
}
fn create_inner(
    config: &Config,
    schema: u32,
    recipient: &str,
    output: &Path,
    operation: &str,
) -> Result<Manifest> {
    config.check_volume()?;
    if !recipient.starts_with("age1")
        || recipient.len() > 128
        || !recipient
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    {
        return Err(Error::Config("backup requires an age X25519 recipient"));
    }
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(output)
        .map_err(|_| Error::Config("backup output must be a new directory"))?;
    let work = tempfile::tempdir().map_err(|_| Error::File)?;
    let mut client = config.client()?;
    let signing_key = config.signing_key()?;
    let snapshot = MaintenanceSnapshot::begin(
        &mut client,
        ByteStore::open(&config.bytes_directory)?,
        &config.instance,
        &signing_key,
        schema,
    )?;
    let database = work.path().join("database.dump");
    let mut dump = Process(
        postgres_command(config, "pg_dump")?
            .args([
                "--format=custom",
                "--compress=none",
                "--no-owner",
                "--no-privileges",
                "--strict-names",
                "--schema=public",
            ])
            .arg(format!("--snapshot={}", snapshot.exported_snapshot()))
            .arg("--file")
            .arg(&database)
            .spawn()
            .map_err(|_| Error::Config("pg_dump unavailable"))?,
    );
    checked(&mut dump)?;
    let payload_path = output.join("payload.tar.age");
    let payload = OpenOptions::new()
        .write(true)
        .read(true)
        .create_new(true)
        .mode(0o600)
        .open(&payload_path)
        .map_err(|_| Error::File)?;
    let mut encrypt = Process(
        Command::new("age")
            .args(["--encrypt", "--recipient", recipient])
            .stdin(Stdio::piped())
            .stdout(payload.try_clone().map_err(|_| Error::File)?)
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| Error::Config("age encryption tool unavailable"))?,
    );
    let mut archive = tar::Builder::new(encrypt.0.stdin.take().ok_or(Error::File)?);
    let mut files = BTreeMap::new();
    files.insert(
        "database.dump".into(),
        append(&mut archive, "database.dump", &mut regular(&database)?)?,
    );
    create_private(&work.path().join("receipt.key"), &signing_key.to_bytes())?;
    files.insert(
        "receipt.key".into(),
        append(
            &mut archive,
            "receipt.key",
            &mut regular(&work.path().join("receipt.key"))?,
        )?,
    );
    create_private(
        &work.path().join("config.json"),
        &serde_json::to_vec(config).map_err(|_| Error::File)?,
    )?;
    files.insert(
        "config.json".into(),
        append(
            &mut archive,
            "config.json",
            &mut regular(&work.path().join("config.json"))?,
        )?,
    );
    for blob in &snapshot.catalog().blobs {
        let name = blob_path(blob);
        let description = append(&mut archive, &name, &mut snapshot.read_verified(blob)?)?;
        if description.blake3 != blob.blake3 || description.bytes != blob.bytes {
            return Err(Error::Config("snapshot bytes changed"));
        }
        files.insert(name, description);
    }
    archive.finish().map_err(|_| Error::File)?;
    drop(archive);
    checked(&mut encrypt)?;
    payload.sync_all().map_err(|_| Error::File)?;
    let catalog = snapshot.catalog().clone();
    drop(snapshot);
    let manifest = Manifest {
        format: 1,
        kind: "disaster_recovery".into(),
        operation_id: Some(operation.into()),
        catalog,
        files,
        payload: digest(&mut regular(&payload_path)?)?,
    };
    let encoded = serde_json::to_vec(&manifest).map_err(|_| Error::File)?;
    let signed = SignedManifest {
        signature: STANDARD.encode(signing_key.sign(&encoded).to_bytes()),
        manifest: manifest.clone(),
    };
    let completed = serde_json::to_vec_pretty(&signed).map_err(|_| Error::File)?;
    if completed.len() > MANIFEST_LIMIT {
        return Err(Error::Config("backup manifest exceeds supported size"));
    }
    // This no-clobber, synced file is the completion marker. Missing or invalid
    // manifests identify an incomplete backup even if the payload exists.
    create_private(&output.join("manifest.json"), &completed)?;
    Ok(manifest)
}

pub struct VerifiedBackup {
    pub manifest: Manifest,
    pub manifest_digest: String,
    directory: tempfile::TempDir,
}
impl VerifiedBackup {
    pub fn path(&self) -> &Path {
        self.directory.path()
    }
}
pub fn verify(
    input: &Path,
    identity: &Path,
    trusted_key: [u8; 32],
    maximum_bytes: u64,
) -> Result<VerifiedBackup> {
    let bytes = read_regular(&input.join("manifest.json"), MANIFEST_LIMIT, false)?;
    let signed: SignedManifest =
        serde_json::from_slice(&bytes).map_err(|_| Error::Config("backup manifest"))?;
    let encoded = serde_json::to_vec(&signed.manifest).map_err(|_| Error::File)?;
    let signature: [u8; 64] = STANDARD
        .decode(&signed.signature)
        .map_err(|_| Error::Config("backup signature"))?
        .try_into()
        .map_err(|_| Error::Config("backup signature"))?;
    VerifyingKey::from_bytes(&trusted_key)
        .map_err(|_| Error::Config("trusted receipt key"))?
        .verify_strict(&encoded, &Signature::from_bytes(&signature))
        .map_err(|_| Error::Config("backup signature does not match trusted receipt key"))?;
    let manifest = signed.manifest;
    if manifest.format != 1
        || manifest.kind != "disaster_recovery"
        || !(1..=alt_server_store::SCHEMA_VERSION).contains(&manifest.catalog.schema)
    {
        return Err(Error::Config("unsupported backup format/schema"));
    }
    let mut expected: std::collections::BTreeSet<String> =
        ["database.dump", "receipt.key", "config.json"]
            .map(String::from)
            .into();
    for blob in &manifest.catalog.blobs {
        alt_remote_protocol::Scope {
            installation_id: manifest.catalog.instance.installation_id.clone(),
            repository_instance_id: blob.repository.clone(),
            recovery_epoch: 1,
        }
        .validate()
        .map_err(|_| Error::Config("backup repository identity"))?;
        if blob.blake3.len() != 64
            || !blob
                .blake3
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || !expected.insert(blob_path(blob))
        {
            return Err(Error::Config("backup blob identity"));
        }
        let entry = manifest
            .files
            .get(&blob_path(blob))
            .ok_or(Error::Config("backup blob missing from manifest"))?;
        if entry.bytes != blob.bytes || entry.blake3 != blob.blake3 {
            return Err(Error::Config("backup blob manifest mismatch"));
        }
    }
    if expected != manifest.files.keys().cloned().collect() {
        return Err(Error::Config("unexpected backup files"));
    }
    let total = manifest
        .files
        .values()
        .try_fold(0u64, |total, file| total.checked_add(file.bytes))
        .ok_or(Error::Config("backup size overflow"))?;
    if total > maximum_bytes
        || manifest.payload.bytes > maximum_bytes.saturating_add(128 * 1024 * 1024)
    {
        return Err(Error::Config("backup exceeds explicit restore size limit"));
    }
    // Resolve and validate the identity locally; do not let age invoke plugins
    // or fetch an SSH identity as an implicit fallback.
    let secret = read_regular(identity, 4096, true)?;
    if !std::str::from_utf8(&secret).is_ok_and(|text| {
        text.lines()
            .filter(|l| !l.starts_with('#') && !l.is_empty())
            .all(|l| l.starts_with("AGE-SECRET-KEY-1"))
    }) {
        return Err(Error::Config(
            "backup requires a private age X25519 identity file",
        ));
    }
    let directory = tempfile::tempdir().map_err(|_| Error::File)?;
    let saved_identity = directory.path().join("age.identity");
    create_private(&saved_identity, &secret)?;
    let mut payload = regular(&input.join("payload.tar.age"))?;
    let actual = digest(&mut payload)?;
    if actual.bytes != manifest.payload.bytes || actual.blake3 != manifest.payload.blake3 {
        return Err(Error::Config("encrypted backup checksum"));
    }
    let mut decrypt = Process(
        Command::new("age")
            .arg("--decrypt")
            .arg("--identity")
            .arg(&saved_identity)
            .stdin(payload)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| Error::Config("age decryption tool unavailable"))?,
    );
    let mut archive = tar::Archive::new(decrypt.0.stdout.take().ok_or(Error::File)?);
    for item in archive
        .entries()
        .map_err(|_| Error::Config("backup archive"))?
    {
        let mut item = item.map_err(|_| Error::Config("backup archive entry"))?;
        let name = String::from_utf8(item.path_bytes().into_owned())
            .map_err(|_| Error::Config("backup archive path"))?;
        if !item.header().entry_type().is_file() || !expected.remove(&name) {
            return Err(Error::Config("unexpected or duplicate archive entry"));
        }
        let expected_file = &manifest.files[&name];
        if item.size() != expected_file.bytes {
            return Err(Error::Config("archive entry length"));
        }
        let destination = directory.path().join(&name);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent).map_err(|_| Error::File)?;
        }
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(destination)
            .map_err(|_| Error::File)?;
        let count = std::io::copy(&mut item, &mut file).map_err(|_| Error::File)?;
        let actual = digest(&mut file)?;
        if count != expected_file.bytes || actual.blake3 != expected_file.blake3 {
            return Err(Error::Config("archive entry checksum"));
        }
        file.sync_all().map_err(|_| Error::File)?;
    }
    // Consume the authenticated stream to EOF: tar's two zero blocks do not
    // alone prove that age validated the final encrypted chunk.
    std::io::copy(&mut archive.into_inner(), &mut std::io::sink()).map_err(|_| Error::File)?;
    checked(&mut decrypt)?;
    if !expected.is_empty() {
        return Err(Error::Config("backup archive is incomplete"));
    }
    let key: [u8; 32] = read_regular(&directory.path().join("receipt.key"), 32, true)?
        .try_into()
        .map_err(|_| Error::Config("backup receipt key"))?;
    if ed25519_dalek::SigningKey::from_bytes(&key)
        .verifying_key()
        .to_bytes()
        != trusted_key
    {
        return Err(Error::Config("backup contains another receipt key"));
    }
    let source: Config = serde_json::from_slice(&read_regular(
        &directory.path().join("config.json"),
        65536,
        true,
    )?)
    .map_err(|_| Error::Config("backup configuration"))?;
    source.validate()?;
    if source.instance != manifest.catalog.instance {
        return Err(Error::Config("backup instance mismatch"));
    }
    std::fs::remove_file(saved_identity).map_err(|_| Error::File)?;
    Ok(VerifiedBackup {
        manifest,
        manifest_digest: blake3::hash(&encoded).to_hex().to_string(),
        directory,
    })
}

/// Restore only into an empty, dedicated DB and absent volume/key paths. The
/// target config already carries the operator-selected new runtime generation,
/// so it cannot serve the intermediate old-generation SQL archive.
pub fn restore(
    target: &Config,
    backup: VerifiedBackup,
    source_fenced: bool,
) -> Result<SnapshotCatalog> {
    if !source_fenced {
        return Err(Error::Config(
            "source must be externally fenced before disaster recovery",
        ));
    }
    target.validate()?;
    let cloud = target.cloud_runtime()?;
    let original = &backup.manifest.catalog;
    if target.instance.installation_id != original.instance.installation_id
        || target.instance.workspace_id != original.instance.workspace_id
        || target.instance.deployment_mode != original.instance.deployment_mode
        || target.instance.runtime_generation
            <= original.instance.runtime_generation.saturating_add(1)
        || original
            .repositories
            .iter()
            .any(|r| r.recovery_epoch >= target.instance.runtime_generation)
    {
        return Err(Error::Config(
            "restore identity or generation; use portable migration for another installation",
        ));
    }
    if target
        .bytes_directory
        .try_exists()
        .map_err(|_| Error::File)?
        || target
            .receipt_key_file
            .try_exists()
            .map_err(|_| Error::File)?
    {
        return Err(Error::Config(
            "restore requires absent target volume and receipt key",
        ));
    }
    let mut client = target.client()?;
    let acquired: bool = client
        .query_one("SELECT pg_try_advisory_lock(715071909111)", &[])
        .map_err(|_| Error::Database)?
        .get(0);
    if !acquired {
        return Err(Error::Config(
            "another target maintenance operation is active",
        ));
    }
    let empty: bool = client.query_one("SELECT NOT EXISTS(SELECT 1 FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname !~ '^pg_' AND n.nspname<>'information_schema')", &[]).map_err(|_| Error::Database)?.get(0);
    if !empty {
        return Err(Error::Config(
            "restore requires an empty dedicated database",
        ));
    }
    let mut stage = "destination files";
    let outcome = (|| -> Result<SnapshotCatalog> {
        // These no-clobber targets are deliberately left in place on failure. A
        // failed/unknown restoration is inspected or discarded as a whole, never
        // retried by overwriting an instance that might have accepted new writes.
        create_private(
            &target.receipt_key_file,
            &read_regular(&backup.path().join("receipt.key"), 32, true)?,
        )?;
        let bytes = ByteStore::open(&target.bytes_directory)?;
        create_private(
            &target.bytes_directory.join("instance.json"),
            &serde_json::to_vec(&target.volume_identity()?).map_err(|_| Error::File)?,
        )?;
        for blob in &original.blobs {
            let scope = alt_remote_protocol::Scope {
                installation_id: original.instance.installation_id.clone(),
                repository_instance_id: blob.repository.clone(),
                recovery_epoch: 1,
            };
            bytes.put(
                &scope,
                &blob.blake3,
                blob.bytes,
                blob.bytes,
                &mut regular(&backup.path().join(blob_path(blob)))?,
            )?;
        }
        stage = "database archive";
        let mut restore = Process(
            postgres_command(target, "pg_restore")?
                .args([
                    "--single-transaction",
                    "--exit-on-error",
                    "--clean",
                    "--if-exists",
                    "--no-owner",
                    "--no-privileges",
                ])
                // pg_restore otherwise defaults to writing a SQL script, even when
                // PGDATABASE is present. An empty conninfo uses that explicit env.
                .arg("--dbname=")
                .arg(backup.path().join("database.dump"))
                .spawn()
                .map_err(|_| Error::Config("pg_restore unavailable"))?,
        );
        checked(&mut restore)?;
        stage = "restored snapshot verification";
        let key = target.signing_key()?;
        {
            let snapshot = MaintenanceSnapshot::begin(
                &mut client,
                bytes.clone(),
                &original.instance,
                &key,
                original.schema,
            )?;
            if snapshot.catalog() != original {
                return Err(Error::Config(
                    "restored catalog differs from signed snapshot",
                ));
            }
            for blob in &original.blobs {
                snapshot.read_verified(blob)?;
            }
        }
        stage = "schema migration";
        let mut store = match original.schema {
            1 => alt_server_store::Store::migrate_v1(
                client,
                bytes,
                original.instance.clone(),
                key,
                target.limits,
            )?,
            2 => alt_server_store::Store::migrate_v2(
                client,
                bytes,
                original.instance.clone(),
                key,
                target.limits,
            )?,
            3 => alt_server_store::Store::migrate_v3(
                client,
                bytes,
                original.instance.clone(),
                key,
                target.limits,
            )?,
            4 => alt_server_store::Store::migrate_v4(
                client,
                bytes,
                original.instance.clone(),
                key,
                target.limits,
            )?,
            _ => alt_server_store::Store::open(
                client,
                bytes,
                original.instance.clone(),
                key,
                target.limits,
            )?,
        };
        stage = "recovery activation";
        if let Some(cloud) = &cloud {
            store.activate_restored_cloud(
                target.instance.runtime_generation,
                &backup.manifest_digest,
                alt_server_store::CloudRestoreBinding {
                    audience: cloud.audience(),
                    runtime_key_digest: cloud.key_digest(),
                    provision_digest: cloud.provision_digest(),
                },
            )?;
        } else {
            store.activate_restored(target.instance.runtime_generation, &backup.manifest_digest)?;
        }
        drop(store);
        stage = "final health verification";
        target.open()?;
        let mut client = target.client()?;
        let snapshot = MaintenanceSnapshot::begin(
            &mut client,
            ByteStore::open(&target.bytes_directory)?,
            &target.instance,
            &target.signing_key()?,
            alt_server_store::SCHEMA_VERSION,
        )?;
        Ok(snapshot.catalog().clone())
    })();
    // The destination may now contain durable state, even if a later check or
    // connection failed. Never label a partially applied restoration not_applied.
    outcome.map_err(|_| Error::RestoreStage(stage))
}
