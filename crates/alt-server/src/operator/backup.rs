//! Encrypted, signed physical backups. The SQL archive and immutable bytes share
//! one fenced database snapshot. A completed manifest is published only after
//! the encrypted payload has been synced. Verification is not a restore drill.
use super::{Config, Error, Result, create_private, read_regular};
use alt_base64::STANDARD;
use alt_crypto::ed25519::{Signature, VerifyingKey};
use alt_server_store::{ByteStore, MaintenanceSnapshot, SnapshotCatalog};
use std::{
    collections::BTreeMap,
    fs::OpenOptions,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::Path,
    process::{Child, Command, Stdio},
};

mod archive;
mod restore;
mod tools;
mod unread;
mod verify;
use archive::{append, blob_path, digest, regular, segment_path};
pub use restore::restore;
use tools::{Process, checked, postgres_command};
pub use unread::Unread;
pub use verify::{VerifiedBackup, verify};

const MANIFEST_LIMIT: usize = 64 * 1024 * 1024;

/// The directory `path` is in, where what a command writes on its way to `path`
/// is put: on the disk `path` is on, not among the temporary files of the host,
/// which may be small or kept in memory, and a backup is as large as the bytes
/// of the instance.
pub fn beside(path: &Path) -> Result<&Path> {
    match path.parent() {
        Some(parent) if parent.as_os_str().is_empty() => Ok(Path::new(".")),
        Some(parent) => Ok(parent),
        None => Err(Error::Config("a directory to write beside")),
    }
}
#[derive(Clone, Debug, alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub struct FileDigest {
    pub bytes: u64,
    pub blake3: String,
}
#[derive(Clone, Debug, alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub struct Manifest {
    pub format: u32,
    pub kind: String,
    #[json(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    pub catalog: SnapshotCatalog,
    pub files: BTreeMap<String, FileDigest>,
    pub payload: FileDigest,
    /// the segments that did not give back all of their objects when the backup
    /// was made, where it was made with such
    #[json(default, skip_serializing_if = "Vec::is_empty")]
    pub unread: Vec<Unread>,
}
#[derive(alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub(crate) struct SignedManifest {
    manifest: Manifest,
    signature: String,
}
/// Physical access to the configuration, receipt key and database is required;
/// an HTTP administrator token alone cannot export passwords or signing keys.
/// The recipient is a public age key. Keep its private identity off the server.
pub fn create(
    config: &Config,
    schema: u32,
    recipient: &str,
    output: &Path,
    with_unread: bool,
) -> Result<Manifest> {
    // The older schema cannot record maintenance events. Its signed
    // completion manifest remains the authority; never migrate just to back up.
    let mut store = if schema == alt_server_store::SCHEMA_VERSION {
        Some(config.open()?)
    } else {
        None
    };
    // what the operator gave is looked at before the objects are read, which
    // takes long
    if !recipient.starts_with("age1")
        || recipient.len() > 128
        || !recipient
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    {
        return Err(Error::Config("backup requires an age X25519 recipient"));
    }
    if output.try_exists().map_err(|_| Error::File)? {
        return Err(Error::Config("backup output must be a new directory"));
    }
    // a copy of segments that do not give their objects back is no backup,
    // unless it is asked for as that
    let unread = match &mut store {
        Some(store) => unread::found(store, "backup")?,
        None => Vec::new(),
    };
    if !unread.is_empty() && !with_unread {
        return Err(unread::refused(
            "backup_created",
            &unread,
            unread::BACKED_UP,
        ));
    }
    let operation = alt_browser_auth::secret();
    if let Some(store) = &mut store {
        store.record_backup_event(&operation, alt_server_store::BackupEvent::Started)?;
    }
    let result = create_inner(config, schema, recipient, output, (&operation, unread));
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
    (operation, unread): (&str, Vec<Unread>),
) -> Result<Manifest> {
    alt_server_store::check_backup_schema(schema).map_err(Error::Config)?;
    config.check_volume()?;
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(output)
        .map_err(|_| Error::Config("backup output must be a new directory"))?;
    let work = alt_os::temp::tempdir_in(beside(output)?).map_err(|_| Error::File)?;
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
    let mut archive = alt_tar::Builder::new(encrypt.0.stdin.take().ok_or(Error::File)?);
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
        &alt_json::to_vec(config).map_err(|_| Error::File)?,
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
    for segment in &snapshot.catalog().pack_segments {
        let name = segment_path(segment);
        let description = append(
            &mut archive,
            &name,
            &mut snapshot.read_pack_segment(segment)?,
        )?;
        if description.blake3 != segment.blake3 || description.bytes != segment.bytes {
            return Err(Error::Config("snapshot bytes changed"));
        }
        files.insert(name, description);
    }
    drop(archive.finish().map_err(|_| Error::File)?);
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
        unread,
    };
    let encoded = alt_json::to_vec(&manifest).map_err(|_| Error::File)?;
    let signed = SignedManifest {
        signature: STANDARD.encode(signing_key.sign(&encoded).to_bytes()),
        manifest: manifest.clone(),
    };
    let completed = alt_json::to_vec_pretty(&signed).map_err(|_| Error::File)?;
    if completed.len() > MANIFEST_LIMIT {
        return Err(Error::Config("backup manifest exceeds supported size"));
    }
    // This no-clobber, synced file is the completion marker. Missing or invalid
    // manifests identify an incomplete backup even if the payload exists.
    create_private(&output.join("manifest.json"), &completed)?;
    Ok(manifest)
}
