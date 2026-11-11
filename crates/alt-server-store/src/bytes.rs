use crate::{Error, Result};
use alt_remote_protocol::{ContentEntry, Scope};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
};

mod batch;
pub use batch::{ByteInput, MAX_PUT_BYTES, MAX_PUT_INPUTS};
mod segments;
pub(crate) use segments::ReadSession;
pub use segments::{DurableSegment, SegmentLocation, StoredReader};

/// A dedicated, service-owned POSIX volume. The adapter never derives a path
/// from a repository display name, ref, URL or arbitrary client filename.
#[derive(Clone)]

pub struct ByteStore {
    root: PathBuf,
}
/// Capacity of the byte-store filesystem, which can be shared with other data.
#[derive(Clone, Copy, Debug)]

pub struct VolumeCapacity {
    pub total_bytes: u64,
    pub available_bytes: u64,
}
/// Verified bytes backed by a synchronized installation, or an existing stored
/// database record. Publication rechecks that record, current location and pin.
pub struct DurableBytes {
    file: DurableFile,
    location: Option<SegmentLocation>,
    segment_checksum: Option<[u8; 32]>,
    scope: Scope,
    blake3: String,
    bytes: u64,
}
enum DurableFile {
    Installed(Arc<File>),
    // This is a validated observation of an already stored DB member, not a
    // claim that metadata itself proves durability or payload integrity.
    Existing(segments::Stamp),
}
impl DurableBytes {
    pub(crate) fn existing(&self) -> bool {
        matches!(self.file, DurableFile::Existing(_))
    }
    fn installed_file(&self) -> Result<&Arc<File>> {
        match &self.file {
            DurableFile::Installed(file) => Ok(file),
            DurableFile::Existing(_) => Err(Error::Integrity("expected installed byte proof")),
        }
    }
    pub fn scope(&self) -> &Scope {
        &self.scope
    }
    pub fn blake3(&self) -> &str {
        &self.blake3
    }
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
    pub fn segment_id(&self) -> Option<&str> {
        self.location
            .as_ref()
            .map(|location| location.segment_id.as_str())
    }
    pub(crate) fn location(&self) -> Option<&SegmentLocation> {
        self.location.as_ref()
    }
    pub(crate) fn segment_checksum(&self) -> Option<&[u8; 32]> {
        self.segment_checksum.as_ref()
    }
}
impl ByteStore {
    /// The parent is provisioned by the operator. New directories are private;
    /// symlinks or non-directories at storage-owned components are rejected.
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }
    pub fn open(root: &Path) -> Result<Self> {
        directory(root)?;
        Ok(Self {
            root: root.to_path_buf(),
        })
    }
    /// Read through a directory descriptor so replacing the final component by
    /// a symlink cannot redirect this observation to an unrelated volume.
    pub fn capacity(&self) -> Result<VolumeCapacity> {
        use std::os::fd::AsRawFd;
        let directory = OpenOptions::new()
            .read(true)
            .custom_flags(
                alt_os::sys::O_NOFOLLOW | alt_os::sys::O_DIRECTORY | alt_os::sys::O_CLOEXEC,
            )
            .open(&self.root)?;
        let mut value = std::mem::MaybeUninit::<alt_os::sys::statvfs>::uninit();
        // SAFETY: fstatvfs initializes the pointed-to structure on success; the
        // descriptor is owned and remains open through this call.
        if unsafe { alt_os::sys::fstatvfs(directory.as_raw_fd(), value.as_mut_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        // SAFETY: the successful call above initialized every output field.
        let value = unsafe { value.assume_init() };
        let scale = |blocks| {
            u64::try_from((value.f_frsize as u128) * blocks)
                .map_err(|_| Error::Limit("volume capacity"))
        };
        let total_bytes = scale(value.f_blocks as u128)?;
        let available_bytes = scale(value.f_bavail as u128)?;
        if available_bytes > total_bytes {
            return Err(Error::Integrity("volume capacity"));
        }
        Ok(VolumeCapacity {
            total_bytes,
            available_bytes,
        })
    }
    /// Probe only service-owned temporary data, after checking the mounted
    /// volume marker. Missing or replaced volumes never become ready.
    pub(crate) fn readiness(&self, spec: &crate::InstanceSpec, key_id: &str) -> Result<()> {
        let mut marker = OpenOptions::new()
            .read(true)
            .custom_flags(alt_os::sys::O_NOFOLLOW | alt_os::sys::O_CLOEXEC)
            .open(self.root.join("instance.json"))?;
        let meta = marker.metadata()?;
        if !meta.is_file() || meta.len() > 4096 || meta.permissions().mode() & 0o077 != 0 {
            return Err(Error::Integrity("volume identity file"));
        }
        let mut bytes = Vec::new();
        Read::by_ref(&mut marker)
            .take(4097)
            .read_to_end(&mut bytes)?;
        let identity: alt_json::Value = alt_json::from_slice(&bytes)?;
        if identity
            != alt_json::json!({"version":1,"installation_id":spec.installation_id,
            "workspace_id":spec.workspace_id,"deployment_mode":spec.deployment_mode,"receipt_key_id":key_id})
        {
            return Err(Error::Integrity("volume identity"));
        }
        let mut probe = alt_os::temp::NamedTempFile::new_in(&self.root)?;
        let mut random = [0u8; 32];
        alt_os::random::fill(&mut random);
        probe.write_all(&random)?;
        probe.as_file().sync_all()?;
        File::open(&self.root)?.sync_all()?;
        let actual = fs::read(probe.path())?;
        if actual != random {
            return Err(Error::Integrity("storage readiness"));
        }
        probe.close()?;
        File::open(&self.root)?.sync_all()?;
        Ok(())
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
        crate::telemetry::scope(scope);
        let mut phase =
            alt_server_telemetry::PhaseGuard::current(alt_server_telemetry::Phase::Persist);
        let result = self.put_inner(scope, digest, bytes, limit, source);
        if result.is_ok() {
            phase.add_bytes(bytes);
        }
        phase.finish(
            result
                .as_ref()
                .map(|_| alt_server_telemetry::Outcome::Ok)
                .unwrap_or_else(crate::telemetry::outcome),
        );
        result
    }
    fn put_inner(
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
        let temp = stage(parent, digest, bytes, source)?;
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
            file: DurableFile::Installed(Arc::new(self.read(scope, digest, bytes)?)),
            location: None,
            segment_checksum: None,
            scope: scope.clone(),
            blake3: digest.into(),
            bytes,
        })
    }
    pub(crate) fn check_proof(&self, proof: &DurableBytes) -> Result<()> {
        if proof.existing() {
            return self.existing_reader(proof).map(|_| ());
        }
        if proof.location.is_some() {
            return self.check_segment_member_identity(proof);
        }
        let current = self
            .read_payload(
                &proof.scope,
                &proof.blake3,
                proof.bytes,
                proof.location.as_ref(),
            )?
            .metadata()?;
        let original = proof.installed_file()?.metadata()?;
        if current.dev() != original.dev() || current.ino() != original.ino() {
            return Err(Error::Precondition("durable object was replaced"));
        }
        Ok(())
    }
    pub fn read(&self, scope: &Scope, digest: &str, bytes: u64) -> Result<File> {
        let path = self.destination(scope, digest)?;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(alt_os::sys::O_NOFOLLOW | alt_os::sys::O_CLOEXEC)
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
        let mut hash = alt_hash::blake3::Hasher::new();
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
    pub(crate) fn verify_chunk_reader(mut file: impl Read, entry: &ContentEntry) -> Result<()> {
        let mut payload = alt_hash::blake3::Hasher::new();
        let mut buffer = [0u8; 65536];
        for chunk in &entry.chunks {
            let mut hash = alt_hash::blake3::Hasher::new();
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
fn stage(
    parent: &Path,
    digest: &str,
    bytes: u64,
    source: &mut dyn Read,
) -> Result<alt_os::temp::NamedTempFile> {
    let mut temp = alt_os::temp::NamedTempFile::new_in(parent)?;
    let mut hash = alt_hash::blake3::Hasher::new();
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
    Ok(temp)
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
