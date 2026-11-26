//! Reading a backup back: its manifest, its signature and every file it names.
use super::*;

/// The identity of `identity`, resolved and validated here: age is not let invoke
/// plugins or fetch an SSH identity as an implicit fallback.
fn age_identity(identity: &Path) -> Result<Vec<u8>> {
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
    Ok(secret)
}

pub struct VerifiedBackup {
    pub manifest: Manifest,
    pub manifest_digest: String,
    directory: alt_os::temp::TempDir,
}
impl VerifiedBackup {
    pub fn path(&self) -> &Path {
        self.directory.path()
    }
}
/// Reads the backup at `input` and its files out into a directory of their own
/// in `work`.
pub fn verify(
    (input, work): (&Path, &Path),
    identity: &Path,
    trusted_key: [u8; 32],
    maximum_bytes: u64,
) -> Result<VerifiedBackup> {
    let bytes = read_regular(&input.join("manifest.json"), MANIFEST_LIMIT, false)?;
    let signed: SignedManifest =
        alt_json::from_slice(&bytes).map_err(|_| Error::Config("backup manifest"))?;
    let encoded = alt_json::to_vec(&signed.manifest).map_err(|_| Error::File)?;
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
    if manifest.format != 1 || manifest.kind != "disaster_recovery" {
        return Err(Error::Config("unsupported backup format"));
    }
    alt_server_store::check_backup_schema(manifest.catalog.schema).map_err(Error::Config)?;
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
    for segment in &manifest.catalog.pack_segments {
        alt_remote_protocol::Scope {
            installation_id: manifest.catalog.instance.installation_id.clone(),
            repository_instance_id: segment.repository.clone(),
            recovery_epoch: 1,
        }
        .validate()
        .map_err(|_| Error::Config("backup repository identity"))?;
        if segment.segment.len() != 32
            || !segment.segment.bytes().all(|b| b.is_ascii_hexdigit())
            || !expected.insert(segment_path(segment))
        {
            return Err(Error::Config("backup pack segment identity"));
        }
        let entry = manifest
            .files
            .get(&segment_path(segment))
            .ok_or(Error::Config("backup pack segment missing from manifest"))?;
        if entry.bytes != segment.bytes || entry.blake3 != segment.blake3 {
            return Err(Error::Config("backup pack segment manifest mismatch"));
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
    let secret = age_identity(identity)?;
    let directory = alt_os::temp::tempdir_in(work).map_err(|_| Error::File)?;
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
    let mut archive = alt_tar::Archive::new(decrypt.0.stdout.take().ok_or(Error::File)?);
    while let Some(mut item) = archive
        .next_entry()
        .map_err(|_| Error::Config("backup archive entry"))?
    {
        let name = String::from_utf8(item.path().to_vec())
            .map_err(|_| Error::Config("backup archive path"))?;
        if item.kind() != alt_tar::Kind::File || !expected.remove(&name) {
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
        let count = item.copy_to(&mut file).map_err(|_| Error::File)?;
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
    if alt_crypto::ed25519::SigningKey::from_bytes(&key)
        .verifying_key()
        .to_bytes()
        != trusted_key
    {
        return Err(Error::Config("backup contains another receipt key"));
    }
    let source: Config = alt_json::from_slice(&read_regular(
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
        manifest_digest: alt_hash::blake3::hash(&encoded).to_hex().to_string(),
        directory,
    })
}
