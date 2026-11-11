use crate::{Error, Result};
use alt_git_codec::{ObjectId, ObjectKind};
use alt_remote_protocol::{
    BeginRequest, Chunk, ContentEntry, ContentKind, Limits, Manifest, ObjectFormat, Publication,
};
use alt_repository_service::{ObjectReader, ObjectSource};
use alt_task::Artifact;
use alt_task_service::{ArtifactSource, TaskLimits, VerifiedPublication};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

/// A private unpublished draft. Only explicitly captured input is included.
pub struct Draft {
    directory: alt_os::temp::TempDir,
    entries: Vec<ContentEntry>,
    limits: Limits,
    total: u64,
}
impl ArtifactSource for Draft {
    fn open(&self, artifact: &Artifact) -> alt_task_service::Result<Box<dyn Read + '_>> {
        if !self
            .entries
            .iter()
            .any(|e| e.blake3 == artifact.blake3 && e.bytes == artifact.bytes)
        {
            return Err(alt_task_service::Error::Integrity(
                "unselected draft artifact",
            ));
        }
        Ok(Box::new(
            regular(&self.directory.path().join("content").join(&artifact.blake3))
                .map_err(|_| alt_task_service::Error::Integrity("draft content unavailable"))?,
        ))
    }
}
impl Draft {
    pub fn new(parent: &Path, limits: Limits) -> Result<Self> {
        limits.validate()?;
        let directory = alt_os::temp::Builder::new()
            .prefix("alt-transfer-")
            .tempdir_in(parent)?;
        std::fs::create_dir(directory.path().join("content"))?;
        Ok(Self {
            directory,
            entries: vec![],
            limits,
            total: 0,
        })
    }
    pub fn entries(&self) -> &[ContentEntry] {
        &self.entries
    }
    pub fn capture(
        &mut self,
        kind: ContentKind,
        git_oid: Option<String>,
        bytes: u64,
        source: &mut dyn Read,
    ) -> Result<ContentEntry> {
        let total = self
            .total
            .checked_add(bytes)
            .ok_or(Error::Limit("package bytes"))?;
        if bytes > self.limits.object_bytes
            || total > self.limits.total_bytes
            || self.entries.len() >= self.limits.manifest_entries
        {
            return Err(Error::Limit("package content"));
        }
        if !matches!(kind, ContentKind::Blob | ContentKind::Attachment)
            && bytes > self.limits.structured_bytes as u64
        {
            return Err(Error::Limit("structured payload"));
        }
        let content = self.directory.path().join("content");
        let mut temporary = alt_os::temp::NamedTempFile::new_in(&content)?;
        let mut hash = alt_hash::blake3::Hasher::new();
        let mut chunks = Vec::new();
        let mut offset = 0;
        let mut buffer = [0u8; 65536];
        while offset < bytes {
            let count = self.limits.chunk_bytes.min(bytes - offset);
            let mut remaining = count;
            let mut chunk_hash = alt_hash::blake3::Hasher::new();
            while remaining > 0 {
                let n = source.read(&mut buffer[..remaining.min(65536) as usize])?;
                if n == 0 {
                    return Err(Error::Invalid("truncated source"));
                }
                temporary.write_all(&buffer[..n])?;
                hash.update(&buffer[..n]);
                chunk_hash.update(&buffer[..n]);
                remaining -= n as u64;
            }
            chunks.push(Chunk {
                offset,
                bytes: count,
                blake3: chunk_hash.finalize().to_hex().to_string(),
            });
            offset += count;
            if chunks.len() > self.limits.request_bytes / 64 {
                return Err(Error::Limit("chunk inventory"));
            }
        }
        if source.read(&mut buffer[..1])? != 0 {
            return Err(Error::Invalid("source grew during capture"));
        }
        let entry = ContentEntry {
            kind,
            git_oid,
            bytes,
            blake3: hash.finalize().to_hex().to_string(),
            chunks,
        };
        Manifest {
            protocol_version: 1,
            object_format: ObjectFormat::Sha1,
            entries: vec![entry.clone()],
        }
        .validate(self.limits)?;
        if let Some(previous) = self
            .entries
            .iter()
            .find(|e| e.kind == entry.kind && e.blake3 == entry.blake3)
        {
            if previous != &entry {
                return Err(Error::Invalid("conflicting content selection"));
            }
            return Ok(previous.clone());
        }
        temporary.as_file().sync_all()?;
        match temporary.persist_noclobber(content.join(&entry.blake3)) {
            Ok(_) => (),
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                verify_file(&content.join(&entry.blake3), &entry)?;
            }
            Err(error) => return Err(error.error.into()),
        }
        self.total = total;
        self.entries.push(entry.clone());
        Ok(entry)
    }
    pub fn capture_bytes(
        &mut self,
        kind: ContentKind,
        git_oid: Option<String>,
        bytes: &[u8],
    ) -> Result<ContentEntry> {
        self.capture(kind, git_oid, bytes.len() as u64, &mut &bytes[..])
    }
    /// Preserve a received manifest's exact chunk boundaries. Its signature
    /// binds that inventory, so a receiver must not silently re-chunk it.
    pub fn capture_selected(&mut self, entry: &ContentEntry, source: &mut dyn Read) -> Result<()> {
        Manifest {
            protocol_version: 1,
            object_format: ObjectFormat::Sha1,
            entries: vec![entry.clone()],
        }
        .validate(self.limits)?;
        let captured = self.capture(entry.kind, entry.git_oid.clone(), entry.bytes, source)?;
        if captured.blake3 != entry.blake3 {
            return Err(Error::Invalid("received content hash"));
        }
        verify_file(
            &self.directory.path().join("content").join(&entry.blake3),
            entry,
        )?;
        let selected = self
            .entries
            .iter_mut()
            .find(|e| e.kind == entry.kind && e.blake3 == entry.blake3)
            .ok_or(Error::Invalid("captured content missing"))?;
        *selected = entry.clone();
        Ok(())
    }
    pub fn manifest(&self) -> Result<Manifest> {
        let manifest = Manifest {
            protocol_version: 1,
            object_format: ObjectFormat::Sha1,
            entries: self.entries.clone(),
        };
        manifest.id(self.limits)?;
        Ok(manifest)
    }
    /// Validate the entire object/task graph, then atomically install an
    /// immutable directory. The caller must supply the exact selected intent.
    pub fn seal(
        self,
        publication: Publication,
        destination: &Path,
        task_limits: TaskLimits,
    ) -> Result<Package> {
        let request = BeginRequest {
            publication,
            manifest: self.manifest()?,
        };
        request
            .manifest
            .validate_publication(&request.publication, self.limits)?;
        let bytes = serde_json::to_vec(&request)?;
        if bytes.len() > self.limits.request_bytes {
            return Err(Error::Limit("begin request bytes"));
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(self.directory.path().join("request.json"))?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        let package = Package::open(self.directory.path(), self.limits)?;
        package.verify(task_limits)?;
        File::open(self.directory.path().join("content"))?.sync_all()?;
        File::open(self.directory.path())?.sync_all()?;
        // Directory rename cannot atomically promise no-clobber portably.
        // Reserve the destination with create_dir, then move only into that
        // owned directory; a crash leaves an explicitly incomplete package.
        std::fs::create_dir(destination)?;
        let result = (|| -> Result<()> {
            std::fs::rename(self.directory.path(), destination.join("sealed"))?;
            File::open(destination)?.sync_all()?;
            if let Some(parent) = destination.parent() {
                File::open(parent)?.sync_all()?;
            }
            Ok(())
        })();
        result?;
        Package::open(&destination.join("sealed"), self.limits)
    }
}

pub struct Package {
    directory: PathBuf,
    request: BeginRequest,
    limits: Limits,
    objects: BTreeMap<ObjectId, usize>,
}
impl Package {
    pub fn open(directory: &Path, limits: Limits) -> Result<Self> {
        limits.validate()?;
        if !std::fs::symlink_metadata(directory)?.is_dir()
            || !std::fs::symlink_metadata(directory.join("content"))?.is_dir()
        {
            return Err(Error::Invalid("package directory"));
        }
        let mut bytes = Vec::new();
        regular(&directory.join("request.json"))?
            .take((limits.request_bytes as u64).saturating_add(1))
            .read_to_end(&mut bytes)?;
        if bytes.len() > limits.request_bytes {
            return Err(Error::Limit("package request bytes"));
        }
        let request: BeginRequest = serde_json::from_slice(&bytes)?;
        request
            .manifest
            .validate_publication(&request.publication, limits)?;
        let mut objects = BTreeMap::new();
        for (i, entry) in request.manifest.entries.iter().enumerate() {
            if let Some(oid) = &entry.git_oid {
                objects.insert(oid.parse().map_err(|_| Error::Invalid("Git object id"))?, i);
            }
        }
        Ok(Self {
            directory: directory.into(),
            request,
            limits,
            objects,
        })
    }
    pub fn request(&self) -> &BeginRequest {
        &self.request
    }
    pub fn directory(&self) -> &Path {
        &self.directory
    }
    pub fn content(&self, digest: &str) -> Result<File> {
        let entry = self
            .request
            .manifest
            .entries
            .iter()
            .find(|entry| entry.blake3 == digest)
            .ok_or(Error::Invalid("unselected content"))?;
        let file = regular(&self.directory.join("content").join(digest))?;
        if file.metadata()?.len() != entry.bytes {
            return Err(Error::Invalid("content length"));
        }
        Ok(file)
    }
    pub fn verify(&self, task_limits: TaskLimits) -> Result<VerifiedPublication> {
        for entry in &self.request.manifest.entries {
            verify_file(&self.directory.join("content").join(&entry.blake3), entry)?;
        }
        Ok(VerifiedPublication::verify(
            &self.request.publication,
            &self.request.manifest,
            self,
            self,
            self.limits,
            task_limits,
        )?)
    }
}
impl ArtifactSource for Package {
    fn open(&self, artifact: &Artifact) -> alt_task_service::Result<Box<dyn Read + '_>> {
        if !self
            .request
            .manifest
            .entries
            .iter()
            .any(|e| e.blake3 == artifact.blake3 && e.bytes == artifact.bytes)
        {
            return Err(alt_task_service::Error::Integrity(
                "unselected package artifact",
            ));
        }
        Ok(Box::new(self.content(&artifact.blake3).map_err(|_| {
            alt_task_service::Error::Integrity("package content unavailable")
        })?))
    }
}
impl ObjectSource for Package {
    fn open(&self, oid: ObjectId) -> alt_repository_service::Result<Option<ObjectReader<'_>>> {
        let Some(index) = self.objects.get(&oid) else {
            return Ok(None);
        };
        let entry = &self.request.manifest.entries[*index];
        let kind = match entry.kind {
            ContentKind::Blob => ObjectKind::Blob,
            ContentKind::Tree => ObjectKind::Tree,
            ContentKind::Commit => ObjectKind::Commit,
            ContentKind::Tag => ObjectKind::Tag,
            _ => {
                return Err(alt_repository_service::Error::Integrity(
                    "package object kind",
                ));
            }
        };
        Ok(Some(ObjectReader {
            kind,
            bytes: entry.bytes,
            reader: Box::new(self.content(&entry.blake3).map_err(|_| {
                alt_repository_service::Error::Integrity("package object unavailable")
            })?),
        }))
    }
}
fn regular(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(alt_os::sys::O_NOFOLLOW | alt_os::sys::O_CLOEXEC)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(Error::Invalid("package content type"));
    }
    Ok(file)
}
fn verify_file(path: &Path, entry: &ContentEntry) -> Result<()> {
    let mut file = regular(path)?;
    if file.metadata()?.len() != entry.bytes {
        return Err(Error::Invalid("content size"));
    }
    let mut hash = alt_hash::blake3::Hasher::new();
    let mut buffer = [0u8; 65536];
    for chunk in &entry.chunks {
        let mut remaining = chunk.bytes;
        let mut chunk_hash = alt_hash::blake3::Hasher::new();
        while remaining > 0 {
            let n = file.read(&mut buffer[..remaining.min(65536) as usize])?;
            if n == 0 {
                return Err(Error::Invalid("truncated content"));
            }
            hash.update(&buffer[..n]);
            chunk_hash.update(&buffer[..n]);
            remaining -= n as u64;
        }
        if chunk_hash.finalize().to_hex().as_str() != chunk.blake3 {
            return Err(Error::Invalid("chunk hash"));
        }
    }
    if file.read(&mut buffer[..1])? != 0 || hash.finalize().to_hex().as_str() != entry.blake3 {
        return Err(Error::Invalid("content hash"));
    }
    Ok(())
}
