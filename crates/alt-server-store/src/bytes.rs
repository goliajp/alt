use crate::{Error, Result};
use alt_remote_protocol::{ContentEntry, Scope};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

/// A dedicated, service-owned POSIX volume. The adapter never derives a path
/// from a repository display name, ref, URL or arbitrary client filename.
#[derive(Clone)]
pub struct ByteStore {
    root: PathBuf,
}
/// Proof that this adapter synced both bytes and directory entry. Publication
/// additionally requires a matching, non-deleting DB row and a live pin.
pub struct DurableBytes {
    file: File,
    scope: Scope,
    blake3: String,
    bytes: u64,
}
impl DurableBytes {
    pub fn scope(&self) -> &Scope {
        &self.scope
    }
    pub fn blake3(&self) -> &str {
        &self.blake3
    }
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
}
impl ByteStore {
    /// The parent is provisioned by the operator. New directories are private;
    /// symlinks or non-directories at storage-owned components are rejected.
    pub fn open(root: &Path) -> Result<Self> {
        directory(root)?;
        Ok(Self {
            root: root.to_path_buf(),
        })
    }
    fn repository(&self, scope: &Scope) -> Result<PathBuf> {
        scope.validate()?;
        let installation = self.root.join(&scope.installation_id);
        directory(&installation)?;
        let repository = installation.join(&scope.repository_instance_id);
        directory(&repository)?;
        Ok(repository)
    }
    fn destination(&self, scope: &Scope, digest: &str) -> Result<PathBuf> {
        check_digest(digest)?;
        Ok(self.repository(scope)?.join(digest))
    }
    /// Streaming bounded ingestion. An extra byte, short input, hash mismatch,
    /// disk error or failed directory sync prevents a durability proof.
    pub fn put(
        &self,
        scope: &Scope,
        digest: &str,
        bytes: u64,
        limit: u64,
        source: &mut dyn Read,
    ) -> Result<DurableBytes> {
        if bytes > limit {
            return Err(Error::Limit("payload bytes"));
        }
        let destination = self.destination(scope, digest)?;
        let parent = destination
            .parent()
            .ok_or(Error::Integrity("storage parent"))?;
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        let mut hash = blake3::Hasher::new();
        let mut remaining = bytes;
        let mut buffer = [0u8; 65536];
        while remaining > 0 {
            let n = source.read(&mut buffer[..remaining.min(65536) as usize])?;
            if n == 0 {
                return Err(Error::Integrity("truncated payload"));
            }
            hash.update(&buffer[..n]);
            temp.write_all(&buffer[..n])?;
            remaining -= n as u64;
        }
        if source.read(&mut buffer[..1])? != 0 {
            return Err(Error::Integrity("payload longer than declared"));
        }
        if hash.finalize().to_hex().as_str() != digest {
            return Err(Error::Integrity("payload hash"));
        }
        temp.as_file().sync_all()?;
        match temp.persist_noclobber(&destination) {
            Ok(file) => {
                file.sync_all()?;
            }
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                // Verify the incumbent; file existence is not a durability or
                // integrity proof, including after a previous process crash.
                self.verify(scope, digest, bytes)?.sync_all()?;
            }
            Err(error) => return Err(error.error.into()),
        }
        File::open(parent)?.sync_all()?;
        Ok(DurableBytes {
            file: self.read(scope, digest, bytes)?,
            scope: scope.clone(),
            blake3: digest.into(),
            bytes,
        })
    }
    pub(crate) fn check_proof(&self, proof: &DurableBytes) -> Result<()> {
        let current = self
            .read(&proof.scope, &proof.blake3, proof.bytes)?
            .metadata()?;
        let original = proof.file.metadata()?;
        if current.dev() != original.dev() || current.ino() != original.ino() {
            return Err(Error::Precondition("durable object was replaced"));
        }
        Ok(())
    }
    pub fn read(&self, scope: &Scope, digest: &str, bytes: u64) -> Result<File> {
        let path = self.destination(scope, digest)?;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.len() != bytes {
            return Err(Error::Integrity("stored payload type or size"));
        }
        Ok(file)
    }
    pub fn verify(&self, scope: &Scope, digest: &str, bytes: u64) -> Result<File> {
        use std::io::Seek;
        let mut file = self.read(scope, digest, bytes)?;
        let mut hash = blake3::Hasher::new();
        let mut buffer = [0u8; 65536];
        let mut remaining = bytes;
        while remaining > 0 {
            let n = file.read(&mut buffer[..remaining.min(65536) as usize])?;
            if n == 0 {
                return Err(Error::Integrity("stored payload truncated"));
            }
            hash.update(&buffer[..n]);
            remaining -= n as u64;
        }
        if file.read(&mut buffer[..1])? != 0 || hash.finalize().to_hex().as_str() != digest {
            return Err(Error::Integrity("stored payload hash"));
        }
        file.rewind()?;
        Ok(file)
    }
    /// The caller validates the manifest's contiguous chunk ranges first.
    /// Both identities matter: chunks describe this request, while the payload
    /// digest identifies the immutable stored object (including empty objects).
    pub(crate) fn verify_content_chunks(&self, scope: &Scope, entry: &ContentEntry) -> Result<()> {
        let mut file = self.read(scope, &entry.blake3, entry.bytes)?;
        let mut payload = blake3::Hasher::new();
        let mut buffer = [0u8; 65536];
        for chunk in &entry.chunks {
            let mut hash = blake3::Hasher::new();
            let mut remaining = chunk.bytes;
            while remaining > 0 {
                let n = file.read(&mut buffer[..remaining.min(65536) as usize])?;
                if n == 0 {
                    return Err(Error::Integrity("truncated chunk"));
                }
                hash.update(&buffer[..n]);
                payload.update(&buffer[..n]);
                remaining -= n as u64;
            }
            if hash.finalize().to_hex().as_str() != chunk.blake3 {
                return Err(Error::Integrity("chunk hash"));
            }
        }
        if file.read(&mut buffer[..1])? != 0 || payload.finalize().to_hex().as_str() != entry.blake3
        {
            return Err(Error::Integrity("stored payload hash"));
        }
        Ok(())
    }
    /// Only the GC coordinator calls this after a committed deleting marker.
    /// The DB marker must remain until this directory sync succeeds. A missing
    /// file is an idempotent retry, not permission to bypass the DB fence.
    pub(crate) fn remove(&self, scope: &Scope, digest: &str) -> Result<()> {
        let destination = self.destination(scope, digest)?;
        match fs::remove_file(&destination) {
            Ok(()) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
        File::open(
            destination
                .parent()
                .ok_or(Error::Integrity("storage parent"))?,
        )?
        .sync_all()?;
        Ok(())
    }
}
fn directory(path: &Path) -> Result<()> {
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {
            File::open(path)?.sync_all()?;
            if let Some(parent) = path.parent() {
                File::open(parent)?.sync_all()?;
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(path)?;
            if !metadata.is_dir() || metadata.permissions().mode() & 0o022 != 0 {
                return Err(Error::Integrity("storage directory type or permissions"));
            }
        }
        Err(error) => return Err(error.into()),
    }
    Ok(())
}
pub(crate) fn check_digest(digest: &str) -> Result<()> {
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Error::Integrity("content address"));
    }
    Ok(())
}
