use crate::{Artifact, Error, Record, Result, check_id};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, Write},
    os::fd::AsRawFd,
    path::{Path, PathBuf},
};

/// A separate private store: no Git object mapping is created here. The lock
/// covers both the blob catalog and immutable record publication.
pub struct Store {
    directory: PathBuf,
    blobs: alt_store::BlobStore,
    _lock: File,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope<T> {
    schema_version: u32,
    hash_algorithm: String,
    kind: String,
    record: T,
}
fn decode<T: Record>(bytes: &[u8]) -> Result<T> {
    let envelope: Envelope<T> = serde_json::from_slice(bytes)?;
    if envelope.schema_version != 1
        || envelope.hash_algorithm != "blake3"
        || envelope.kind != T::KIND
    {
        return Err(Error::Invalid("unsupported record envelope".into()));
    }
    envelope.record.validate()?;
    Ok(envelope.record)
}
impl Store {
    pub fn open(directory: &Path) -> Result<Self> {
        fs::create_dir_all(directory)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(directory.join("lock"))?;
        loop {
            if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } == 0 {
                break;
            }
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(error.into());
            }
        }
        fs::create_dir_all(directory.join("records"))?;
        let blobs = alt_store::BlobStore::open(directory.join("blobs"))?;
        File::open(directory.join("blobs"))?.sync_all()?;
        File::open(directory)?.sync_all()?;
        if let Some(parent) = directory.parent() {
            File::open(parent)?.sync_all()?;
        }
        Ok(Self {
            directory: directory.into(),
            blobs,
            _lock: lock,
        })
    }
    pub fn put_blob(&mut self, bytes: &[u8]) -> Result<Artifact> {
        let id = self.blobs.put(bytes)?;
        Ok(Artifact {
            blake3: id.to_string(),
            bytes: bytes.len() as u64,
        })
    }
    /// Spool mutable input into a private file before mapping it. Never mmap a
    /// caller's live file, which could be truncated while CDC reads it.
    pub fn capture(&mut self, input: &mut dyn Read) -> Result<Artifact> {
        let mut temporary = tempfile::tempfile_in(&self.directory)?;
        let bytes = std::io::copy(input, &mut temporary)?;
        temporary.flush()?;
        temporary.rewind()?;
        if bytes == 0 {
            return self.put_blob(b"");
        }
        let map = unsafe { memmap2::Mmap::map(&temporary)? };
        self.put_blob(&map)
    }
    pub fn blob(&self, artifact: &Artifact) -> Result<Vec<u8>> {
        check_id(&artifact.blake3)?;
        let id = alt_input::unhex(&artifact.blake3)?;
        let bytes = self
            .blobs
            .get(alt_store::BlobId(id.try_into().expect("checked 32 bytes")))?;
        if bytes.len() as u64 != artifact.bytes {
            return Err(Error::Invalid("artifact length mismatch".into()));
        }
        Ok(bytes)
    }
    pub fn verify_blob(&self, artifact: &Artifact) -> Result<()> {
        self.blob(artifact).map(|_| ())
    }
    pub fn publish<T: Record>(&mut self, record: &T) -> Result<String> {
        let id = record.id()?;
        let bytes = serde_json::to_vec(&Envelope {
            schema_version: 1,
            hash_algorithm: "blake3".into(),
            kind: T::KIND.into(),
            record,
        })?;
        let directory = self.directory.join("records").join(T::KIND);
        fs::create_dir_all(&directory)?;
        let destination = directory.join(format!("{id}.json"));
        if destination.exists() {
            let previous: T = decode(&fs::read(&destination)?)?;
            if previous.id()? != id {
                return Err(Error::Invalid("immutable record content mismatch".into()));
            }
            return Ok(id);
        }
        self.blobs.fsync()?;
        let mut file = tempfile::NamedTempFile::new_in(&directory)?;
        file.write_all(&bytes)?;
        file.as_file().sync_all()?;
        file.persist_noclobber(&destination)
            .map_err(|error| error.error)?;
        File::open(&directory)?.sync_all()?;
        File::open(self.directory.join("records"))?.sync_all()?;
        Ok(id)
    }
    pub fn load<T: Record>(&self, id: &str) -> Result<T> {
        check_id(id)?;
        let record: T = decode(&fs::read(
            self.directory
                .join("records")
                .join(T::KIND)
                .join(format!("{id}.json")),
        )?)?;
        if record.id()? != id {
            return Err(Error::Invalid(
                "record content does not match its address".into(),
            ));
        }
        Ok(record)
    }
    pub fn list<T: Record>(&self) -> Result<Vec<(String, T)>> {
        let directory = self.directory.join("records").join(T::KIND);
        let mut records = Vec::new();
        let entries = match fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(records),
            Err(error) => return Err(error.into()),
        };
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name();
            let Some(id) = name.to_str().and_then(|name| name.strip_suffix(".json")) else {
                continue;
            };
            records.push((id.into(), self.load(id)?));
        }
        records.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(records)
    }
}
