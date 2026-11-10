//! A private local journal records immutable migration facts. It is separate
//! from repository requests and never contains credentials or active grants.
use super::{Res, refused};
use alt_portable::{Blob, BlobSource};
use serde::{Serialize, de::DeserializeOwned};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
};

pub struct Journal {
    root: PathBuf,
    _lock: File,
}
fn uid() -> u32 {
    // SAFETY: geteuid has no pointer or lifetime requirements.
    unsafe { libc::geteuid() }
}
fn directory(path: &Path) -> Res<()> {
    match std::fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => File::open(
            path.parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new(".")),
        )?
        .sync_all()?,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    let meta = std::fs::symlink_metadata(path)?;
    if !meta.is_dir() || meta.uid() != uid() || meta.mode() & 0o077 != 0 {
        return Err(refused(
            "migration journal must be an owned private directory",
        ));
    }
    Ok(())
}
fn regular(path: &Path, maximum: u64) -> Res<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.uid() != uid() || meta.mode() & 0o077 != 0 || meta.len() > maximum {
        return Err(refused("invalid private migration file"));
    }
    Ok(file)
}
fn name(value: &str) -> Res<()> {
    if value.is_empty()
        || value.len() > 80
        || !value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.')
        || value.starts_with('.')
        || value.contains("..")
    {
        return Err(refused("invalid migration journal entry"));
    }
    Ok(())
}
fn digest(value: &str) -> Res<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(refused("invalid migration content identity"));
    }
    Ok(())
}
fn verify(mut input: File, blob: &Blob) -> Res<File> {
    use std::io::Seek;
    if input.metadata()?.len() != blob.bytes {
        return Err(refused("local migration content length changed"));
    }
    let mut hash = alt_hash::blake3::Hasher::new();
    let mut remaining = blob.bytes;
    let mut buffer = [0u8; 65536];
    while remaining > 0 {
        let count = input.read(&mut buffer[..remaining.min(65536) as usize])?;
        if count == 0 {
            return Err(refused("local migration content truncated"));
        }
        hash.update(&buffer[..count]);
        remaining -= count as u64;
    }
    if input.read(&mut [0])? != 0 || hash.finalize().to_hex().as_str() != blob.blake3 {
        return Err(refused("local migration content digest changed"));
    }
    input.rewind()?;
    Ok(input)
}
impl Journal {
    pub fn open(path: &Path) -> Res<Self> {
        directory(path)?;
        let root = std::fs::canonicalize(path)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(root.join("lock"))?;
        let meta = lock.metadata()?;
        if !meta.is_file() || meta.uid() != uid() || meta.mode() & 0o077 != 0 {
            return Err(refused("migration journal lock must be private"));
        }
        // SAFETY: a live owned file descriptor; no pointers. Never block an
        // interactive process behind another invocation of the same journal.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(refused("another process is using this migration journal"));
        }
        directory(&root.join("content"))?;
        Ok(Self { root, _lock: lock })
    }
    pub fn read<T: DeserializeOwned>(&self, entry: &str, maximum: usize) -> Res<Option<T>> {
        name(entry)?;
        let mut file = match regular(&self.root.join(entry), maximum as u64) {
            Ok(file) => file,
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take((maximum as u64).saturating_add(1))
            .read_to_end(&mut bytes)?;
        if bytes.len() > maximum {
            return Err(refused("migration journal entry exceeds its budget"));
        }
        Ok(Some(serde_json::from_slice(&bytes)?))
    }
    /// Idempotent only for the same bytes. Updating a pending operation never
    /// rewrites the original identity or overwrites an earlier durable result.
    pub fn record(&self, entry: &str, value: &impl Serialize, maximum: usize) -> Res<()> {
        name(entry)?;
        let bytes = serde_json::to_vec(value)?;
        if bytes.len() > maximum {
            return Err(refused("migration journal entry exceeds its budget"));
        }
        let path = self.root.join(entry);
        match regular(&path, maximum as u64) {
            Ok(file) => {
                let mut old = Vec::new();
                file.take((maximum as u64).saturating_add(1))
                    .read_to_end(&mut old)?;
                if old != bytes {
                    return Err(refused(
                        "migration journal already binds different parameters",
                    ));
                }
                File::open(&self.root)?.sync_all()?;
                return Ok(());
            }
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) => {}
            Err(error) => return Err(error),
        }
        let mut temp = tempfile::NamedTempFile::new_in(&self.root)?;
        temp.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))?;
        temp.write_all(&bytes)?;
        temp.as_file().sync_all()?;
        temp.persist_noclobber(path)?;
        File::open(&self.root)?.sync_all()?;
        Ok(())
    }
    /// The archive footer and every content digest must verify before any
    /// destination request is created. Interrupted private files are temporary.
    pub fn unpack(
        &self,
        path: &Path,
        expected: &alt_remote_protocol::Scope,
        key: &[u8; 32],
        limits: alt_portable::PackageLimits,
    ) -> Res<(alt_portable::Envelope, Blob)> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)?;
        let maximum = limits
            .total_bytes
            .checked_add(limits.metadata_bytes as u64)
            .and_then(|v| v.checked_add(72))
            .ok_or_else(|| refused("archive budget overflow"))?;
        if !file.metadata()?.is_file() || file.metadata()?.len() > maximum {
            return Err(refused(
                "archive exceeds its budget or is not a regular file",
            ));
        }
        let staging = tempfile::Builder::new()
            .prefix("archive-stage-")
            .tempdir_in(&self.root)?;
        let mut reader = ArchiveReader {
            file,
            hash: alt_hash::blake3::Hasher::new(),
            bytes: 0,
        };
        let mut files = Vec::new();
        let envelope = alt_portable::read_archive(&mut reader, expected, key, limits, |blob| {
            let has = self
                .has_content(blob)
                .map_err(|e| alt_portable::Error::Io(std::io::Error::other(e.to_string())))?;
            if has {
                return Ok(Box::new(std::io::sink()));
            }
            let temporary = tempfile::NamedTempFile::new_in(staging.path())?;
            let writer = temporary.as_file().try_clone()?;
            files.push((blob.clone(), temporary.into_temp_path()));
            Ok(Box::new(writer))
        })?;
        let archive = Blob {
            blake3: reader.hash.finalize().to_hex().to_string(),
            bytes: reader.bytes,
        };
        let directory = self.root.join("content");
        for (blob, temporary) in files {
            let file = regular(&temporary, blob.bytes)?;
            file.sync_all()?;
            verify(file, &blob)?;
            temporary.persist_noclobber(directory.join(&blob.blake3))?;
        }
        File::open(&directory)?.sync_all()?;
        Ok((envelope, archive))
    }
    pub fn content(&self, blob: &Blob) -> Res<File> {
        digest(&blob.blake3)?;
        verify(
            regular(&self.root.join("content").join(&blob.blake3), blob.bytes)?,
            blob,
        )
    }
    pub fn has_content(&self, blob: &Blob) -> Res<bool> {
        match self.content(blob) {
            Ok(_) => Ok(true),
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }
    pub fn receive(&self, blob: &Blob, write: impl FnOnce(&mut File) -> Res<()>) -> Res<()> {
        digest(&blob.blake3)?;
        if self.has_content(blob)? {
            return Ok(());
        }
        let dir = self.root.join("content");
        let mut temp = tempfile::NamedTempFile::new_in(&dir)?;
        temp.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))?;
        write(temp.as_file_mut())?;
        temp.as_file_mut().sync_all()?;
        // Reopen instead of sharing a file offset with the writer.
        verify(regular(temp.path(), blob.bytes)?, blob)?;
        temp.persist_noclobber(dir.join(&blob.blake3))?;
        File::open(&dir)?.sync_all()?;
        Ok(())
    }
}
impl BlobSource for Journal {
    fn open(&self, blob: &Blob) -> alt_portable::Result<Box<dyn Read + '_>> {
        self.content(blob)
            .map(|f| Box::new(f) as Box<dyn Read>)
            .map_err(|error| alt_portable::Error::Io(std::io::Error::other(error.to_string())))
    }
}

struct ArchiveReader {
    file: File,
    hash: alt_hash::blake3::Hasher,
    bytes: u64,
}
impl Read for ArchiveReader {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let count = self.file.read(buffer)?;
        self.hash.update(&buffer[..count]);
        self.bytes = self
            .bytes
            .checked_add(count as u64)
            .ok_or_else(|| std::io::Error::other("archive length overflow"))?;
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    #[test]
    fn private_journal_rejects_aliases_conflicts_and_modified_content() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("journal");
        let journal = Journal::open(&path).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o700
        );
        journal
            .record("intent.json", &serde_json::json!({"id":"original"}), 1000)
            .unwrap();
        journal
            .record("intent.json", &serde_json::json!({"id":"original"}), 1000)
            .unwrap();
        assert!(
            journal
                .record("intent.json", &serde_json::json!({"id":"different"}), 1000)
                .is_err()
        );
        assert!(Journal::open(&path).is_err());
        assert!(journal.record("../escape", &true, 1000).is_err());
        let bytes = b"original selected bytes";
        let blob = Blob::of(bytes);
        assert!(
            journal
                .receive(&blob, |file| {
                    file.write_all(b"corrupted")?;
                    Ok(())
                })
                .is_err()
        );
        assert!(!journal.has_content(&blob).unwrap());
        journal
            .receive(&blob, |file| {
                file.write_all(bytes)?;
                Ok(())
            })
            .unwrap();
        journal
            .receive(&blob, |_| panic!("verified local content must be reused"))
            .unwrap();
        std::fs::write(path.join("content").join(&blob.blake3), b"replaced").unwrap();
        assert!(journal.content(&blob).is_err());
        drop(journal);
        symlink(&path, root.path().join("alias")).unwrap();
        assert!(Journal::open(&root.path().join("alias")).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(Journal::open(&path).is_err());
    }

    #[test]
    fn killed_writer_releases_journal_without_exposing_partial_content() {
        const CHILD: &str = "ALT_PORTABLE_JOURNAL_TEST_CHILD";
        let bytes = b"complete immutable migration content";
        let blob = Blob::of(bytes);
        if let Some(path) = std::env::var_os(CHILD) {
            let path = PathBuf::from(path);
            let journal = Journal::open(&path).unwrap();
            journal
                .record("intent.json", &"original-request", 128)
                .unwrap();
            journal
                .receive(&blob, |file| {
                    file.write_all(&bytes[..7])?;
                    file.sync_all()?;
                    std::fs::write(path.join("ready"), b"partial-file-durable")?;
                    loop {
                        std::thread::sleep(Duration::from_secs(1));
                    }
                })
                .unwrap();
            unreachable!();
        }
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("journal");
        let mut child=Command::new(std::env::current_exe().unwrap())
            .args(["--exact","portable::local::tests::killed_writer_releases_journal_without_exposing_partial_content","--nocapture"])
            .env(CHILD,&path).stdout(Stdio::null()).stderr(Stdio::piped()).spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        while !path.join("ready").exists() {
            if let Some(status) = child.try_wait().unwrap() {
                panic!("journal child exited before interruption: {status}");
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("journal child did not reach durable partial write");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        child.kill().unwrap();
        assert!(!child.wait().unwrap().success());
        let journal = Journal::open(&path).unwrap();
        assert_eq!(
            journal.read::<String>("intent.json", 128).unwrap().unwrap(),
            "original-request"
        );
        assert!(!journal.has_content(&blob).unwrap());
        journal
            .receive(&blob, |file| {
                file.write_all(bytes)?;
                Ok(())
            })
            .unwrap();
        let mut recovered = Vec::new();
        journal
            .content(&blob)
            .unwrap()
            .read_to_end(&mut recovered)
            .unwrap();
        assert_eq!(recovered, bytes);
        assert!(
            journal
                .record("intent.json", &"replacement-request", 128)
                .is_err()
        );
    }

    #[test]
    fn archive_staging_bounds_descriptors_and_refuses_incomplete_footer() {
        const CHILD: &str = "ALT_PORTABLE_ARCHIVE_FD_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output=Command::new(std::env::current_exe().unwrap())
                .args(["--exact","portable::local::tests::archive_staging_bounds_descriptors_and_refuses_incomplete_footer","--nocapture"])
                .env(CHILD,"1").output().unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        // SAFETY: only this dedicated test subprocess is affected. The pointer
        // names a live initialized rlimit for the duration of the syscall.
        let limit = libc::rlimit {
            rlim_cur: 64,
            rlim_max: 64,
        };
        assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) }, 0);
        let root = tempfile::tempdir().unwrap();
        let source = Journal::open(&root.path().join("source")).unwrap();
        let mut blobs = Vec::new();
        for i in 0..128 {
            let bytes = format!("selected payload {i}");
            let blob = Blob::of(bytes.as_bytes());
            source
                .receive(&blob, |file| {
                    file.write_all(bytes.as_bytes())?;
                    Ok(())
                })
                .unwrap();
            blobs.push(blob);
        }
        blobs.sort_by(|a, b| a.blake3.cmp(&b.blake3));
        let limits = alt_portable::PackageLimits {
            metadata_bytes: 1_000_000,
            entries: 1000,
            publications: 100,
            blob_bytes: 100_000,
            total_bytes: 1_000_000,
            protocol: alt_remote_protocol::Limits {
                request_bytes: 20000,
                ref_updates: 32,
                record_roots: 32,
                manifest_entries: 1000,
                graph_objects: 1000,
                graph_edges: 10000,
                structured_bytes: 100000,
                tree_paths: 1000,
                path_bytes: 4096,
                walk_steps: 10000,
                chunk_bytes: 100000,
                object_bytes: 100000,
                total_bytes: 1000000,
            },
        };
        let key = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let scope = alt_remote_protocol::Scope {
            installation_id: "source".into(),
            repository_instance_id: "repository".into(),
            recovery_epoch: 1,
        };
        let envelope = alt_portable::Envelope::sign(
            alt_portable::Catalog {
                version: 1,
                source: scope.clone(),
                source_mode: alt_remote_protocol::DeploymentMode::SelfHosted,
                source_public_key: crate::machine::hex(key.verifying_key().as_bytes()),
                export_id: "export".into(),
                snapshot_sequence: 1,
                object_format: alt_remote_protocol::ObjectFormat::Sha1,
                default_branch: "refs/heads/main".into(),
                protections: vec![],
                refs: vec![],
                objects: vec![],
                publications: vec![],
                import_receipts: vec![],
                change_links: vec![],
                identities: vec![],
                blobs: blobs.clone(),
            },
            &key,
            limits,
        )
        .unwrap();
        let path = root.path().join("archive");
        let mut archive = File::create(&path).unwrap();
        alt_portable::write_archive(&mut archive, &envelope, &source, limits).unwrap();
        drop(archive);
        drop(source);
        let incomplete = root.path().join("incomplete");
        std::fs::copy(&path, &incomplete).unwrap();
        let truncated = OpenOptions::new().write(true).open(&incomplete).unwrap();
        truncated
            .set_len(truncated.metadata().unwrap().len() - 16)
            .unwrap();
        drop(truncated);
        let target = Journal::open(&root.path().join("target")).unwrap();
        assert!(
            target
                .unpack(&incomplete, &scope, key.verifying_key().as_bytes(), limits)
                .is_err()
        );
        assert_eq!(
            std::fs::read_dir(root.path().join("target/content"))
                .unwrap()
                .count(),
            0
        );
        target
            .unpack(&path, &scope, key.verifying_key().as_bytes(), limits)
            .unwrap();
        for blob in &blobs {
            assert!(target.has_content(blob).unwrap());
        }
        target
            .unpack(&path, &scope, key.verifying_key().as_bytes(), limits)
            .unwrap();
    }
}
