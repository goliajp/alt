use super::*;
use alt_byte_segment::{Index, Input, Limits, Segment};
use std::{
    io::{self, Seek, SeekFrom},
    os::unix::fs::FileExt,
    sync::Arc,
};

mod session;
pub(crate) use session::ReadSession;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Stamp {
    dev: u64,
    ino: u64,
    bytes: u64,
    mtime: (i64, i64),
    ctime: (i64, i64),
}
impl Stamp {
    fn read(file: &File) -> Result<Self> {
        let m = file.metadata()?;
        if !m.is_file() {
            return Err(Error::Integrity("segment file type"));
        }
        Ok(Self {
            dev: m.dev(),
            ino: m.ino(),
            bytes: m.len(),
            mtime: (m.mtime(), m.mtime_nsec()),
            ctime: (m.ctime(), m.ctime_nsec()),
        })
    }
}

/// A physical member selected by the authoritative database transaction.
/// These values grant no permission to read or publish an object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SegmentLocation {
    pub segment_id: String,
    pub index_digest: [u8; 32],
    pub offset: u64,
}

/// Installed, synchronized bytes. A database transaction must separately check
/// the registered intent, current authorization, and logical member identities.
pub struct DurableSegment {
    file: Arc<File>,
    stamp: Stamp,
    scope: Scope,
    id: String,
    index: Arc<Index>,
    checksum: [u8; 32],
}
impl DurableSegment {
    pub fn scope(&self) -> &Scope {
        &self.scope
    }
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn index(&self) -> &Index {
        &self.index
    }
    /// Member proofs share the one already synchronized backing descriptor.
    /// Registration must still match the segment's authoritative database intent.
    pub fn proofs(&self) -> Vec<DurableBytes> {
        self.index
            .members()
            .iter()
            .map(|member| DurableBytes {
                file: DurableFile::Installed(self.file.clone()),
                scope: self.scope.clone(),
                blake3: alt_hash::blake3::Hash::from(*member.digest())
                    .to_hex()
                    .to_string(),
                bytes: member.bytes(),
                location: self.location(member.digest()),
                segment_checksum: Some(self.checksum),
            })
            .collect()
    }
    pub fn location(&self, digest: &[u8; 32]) -> Option<SegmentLocation> {
        self.index.find(digest).map(|member| SegmentLocation {
            segment_id: self.id.clone(),
            index_digest: *self.index.digest(),
            offset: member.offset(),
        })
    }
}

/// A bounded logical payload. Independent positional reads allow thousands of
/// member proofs/readers to share one descriptor without sharing a seek cursor.
pub struct StoredReader {
    file: Arc<File>,
    start: u64,
    bytes: u64,
    position: u64,
}
impl StoredReader {
    /// Bound an independent complete temporary file for transport. Segment
    /// members are constructed only through the checked location readers.
    pub fn from_file(file: File, bytes: u64) -> io::Result<Self> {
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.len() != bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "temporary payload extent",
            ));
        }
        Ok(Self {
            file: Arc::new(file),
            start: 0,
            bytes,
            position: 0,
        })
    }
    /// Transfer a uniquely owned descriptor positioned at this logical cursor.
    /// The descriptor is physically unbounded: transports must enforce `bytes`
    /// separately (for example a fixed-length HTTP body).
    pub fn into_file(self) -> io::Result<File> {
        let mut file = Arc::try_unwrap(self.file)
            .map_err(|_| io::Error::other("shared stored reader cannot transfer its descriptor"))?;
        file.seek(SeekFrom::Start(self.start + self.position))?;
        Ok(file)
    }
    /// Coalesce descriptors only inside one bounded, authorized operation.
    /// Each reader has already opened and verified its current path and range.
    pub(crate) fn share_backing(&mut self, other: &Self) -> Result<()> {
        if Stamp::read(&self.file)? != Stamp::read(&other.file)? {
            return Err(Error::Precondition("segment backing changed"));
        }
        self.file = other.file.clone();
        Ok(())
    }
    pub fn metadata(&self) -> io::Result<fs::Metadata> {
        self.file.metadata()
    }
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
}
impl Read for StoredReader {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let capacity = (self.bytes - self.position).min(output.len() as u64) as usize;
        if capacity == 0 {
            return Ok(0);
        }
        let n = self
            .file
            .read_at(&mut output[..capacity], self.start + self.position)?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "stored member truncated",
            ));
        }
        self.position += n as u64;
        Ok(n)
    }
}
impl Seek for StoredReader {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let position = match from {
            SeekFrom::Start(v) => i128::from(v),
            SeekFrom::Current(v) => i128::from(self.position) + i128::from(v),
            SeekFrom::End(v) => i128::from(self.bytes) + i128::from(v),
        };
        if position < 0 || position > i128::from(self.bytes) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek outside stored member",
            ));
        }
        self.position = position as u64;
        Ok(self.position)
    }
}

#[derive(Clone, Copy, Debug)]
enum Boundary {
    Created,
    Encoded,
    TemporarySynced,
    Installed,
    InstalledSynced,
    DirectorySynced,
}

impl ByteStore {
    pub(crate) fn check_segment_member_identity(&self, proof: &DurableBytes) -> Result<()> {
        let location = proof
            .location
            .as_ref()
            .ok_or(Error::Integrity("segment member proof"))?;
        let current = open_segment(&self.segment_path(&proof.scope, &location.segment_id)?)?;
        let a = current.metadata()?;
        let b = proof.installed_file()?.metadata()?;
        if a.dev() != b.dev() || a.ino() != b.ino() || a.len() != b.len() {
            return Err(Error::Precondition("segment member was replaced"));
        }
        Ok(())
    }
    pub(crate) fn verify_segment_layout(
        &self,
        scope: &Scope,
        id: &str,
        index: &[u8; 32],
        expected: &std::collections::BTreeMap<String, u64>,
        checksum: &[u8; 32],
    ) -> Result<()> {
        let mut segment = Segment::open(
            open_segment(&self.segment_path(scope, id)?)?,
            Limits::default(),
        )
        .map_err(segment_error)?;
        let actual: std::collections::BTreeMap<_, _> = segment
            .index()
            .members()
            .iter()
            .map(|member| {
                (
                    alt_hash::blake3::Hash::from(*member.digest())
                        .to_hex()
                        .to_string(),
                    member.bytes(),
                )
            })
            .collect();
        if segment.index().digest() != index || &actual != expected {
            return Err(Error::Integrity("registered segment layout"));
        }
        segment.verify_checksum(checksum).map_err(segment_error)
    }
    pub(crate) fn read_payload(
        &self,
        scope: &Scope,
        digest: &str,
        bytes: u64,
        location: Option<&SegmentLocation>,
    ) -> Result<StoredReader> {
        match location {
            Some(location) => self.read_segment(scope, digest, bytes, location),
            None => Ok(StoredReader {
                file: Arc::new(self.read(scope, digest, bytes)?),
                start: 0,
                bytes,
                position: 0,
            }),
        }
    }
    pub(crate) fn verify_payload(
        &self,
        scope: &Scope,
        digest: &str,
        bytes: u64,
        location: Option<&SegmentLocation>,
    ) -> Result<StoredReader> {
        match location {
            Some(location) => self.verify_segment_member(scope, digest, bytes, location),
            None => Ok(StoredReader {
                file: Arc::new(self.verify(scope, digest, bytes)?),
                start: 0,
                bytes,
                position: 0,
            }),
        }
    }
    /// Validate the supplied bytes against the authoritative stored member.
    /// Existing durability comes from its DB record; registration must recheck
    /// that record and location under the repository barrier.
    pub(crate) fn existing_proof(
        &self,
        scope: &Scope,
        input: &mut ByteInput<'_>,
        location: Option<&SegmentLocation>,
    ) -> Result<DurableBytes> {
        let mut reader = self.read_payload(scope, input.digest, input.bytes, location)?;
        let before = Stamp::read(&reader.file)?;
        let mut actual = [0; 65536];
        let mut supplied = [0; 65536];
        let mut remaining = input.bytes;
        let mut hash = alt_hash::blake3::Hasher::new();
        while remaining != 0 {
            let n = remaining.min(actual.len() as u64) as usize;
            reader.read_exact(&mut actual[..n])?;
            input.source.read_exact(&mut supplied[..n])?;
            if actual[..n] != supplied[..n] {
                return Err(Error::Integrity("existing content differs from input"));
            }
            hash.update(&actual[..n]);
            remaining -= n as u64;
        }
        if input.source.read(&mut supplied[..1])? != 0
            || hash.finalize().to_hex().as_str() != input.digest
        {
            return Err(Error::Integrity("existing content identity"));
        }
        if before != Stamp::read(&reader.file)? {
            return Err(Error::Precondition(
                "existing content changed during verification",
            ));
        }
        let proof = DurableBytes {
            file: DurableFile::Existing(before),
            location: location.cloned(),
            segment_checksum: None,
            scope: scope.clone(),
            blake3: input.digest.into(),
            bytes: input.bytes,
        };
        self.check_proof(&proof)?;
        Ok(proof)
    }
    // Registration reopens and validates actual content through read_proof;
    // the final transaction also checks the DB's stored state and exact location.
    // No descriptor per legacy loose member survives between those operations.
    pub(super) fn existing_reader(&self, proof: &DurableBytes) -> Result<StoredReader> {
        let DurableFile::Existing(stamp) = &proof.file else {
            return Err(Error::Integrity("expected existing byte observation"));
        };
        let reader =
            self.read_payload(&proof.scope, &proof.blake3, proof.bytes, proof.location())?;
        if Stamp::read(&reader.file)? != *stamp {
            return Err(Error::Precondition(
                "existing content was replaced or changed",
            ));
        }
        Ok(reader)
    }
    pub(crate) fn read_proof(&self, proof: &DurableBytes) -> Result<StoredReader> {
        if proof.existing() {
            return self.existing_reader(proof);
        }
        self.check_proof(proof)?;
        Ok(StoredReader {
            file: proof.installed_file()?.clone(),
            start: proof.location.as_ref().map_or(0, |l| l.offset),
            bytes: proof.bytes,
            position: 0,
        })
    }
    fn segment_path(&self, scope: &Scope, id: &str) -> Result<PathBuf> {
        check_segment_id(id)?;
        Ok(self.repository(scope)?.join(format!("segment-{id}")))
    }

    /// The caller must commit a bounded database intent for this ID before any
    /// file is created. Inputs must be distinct. An interrupted pending file is
    /// reclaimed only by the coordinator after fencing its registered owner.
    pub fn put_segment(
        &self,
        scope: &Scope,
        id: &str,
        inputs: &mut [Input<'_>],
    ) -> Result<DurableSegment> {
        crate::telemetry::scope(scope);
        let mut phase =
            alt_server_telemetry::PhaseGuard::current(alt_server_telemetry::Phase::Persist);
        let result = self.put_segment_inner(scope, id, inputs, |_| Ok(()));
        if let Ok(proof) = &result {
            phase.add_bytes(proof.index().bytes());
        }
        phase.finish(
            result
                .as_ref()
                .map(|_| alt_server_telemetry::Outcome::Ok)
                .unwrap_or_else(crate::telemetry::outcome),
        );
        result
    }
    fn put_segment_inner(
        &self,
        scope: &Scope,
        id: &str,
        inputs: &mut [Input<'_>],
        boundary: impl Fn(Boundary) -> io::Result<()>,
    ) -> Result<DurableSegment> {
        // Validate metadata before touching the volume or invoking a source.
        scope.validate()?;
        check_segment_id(id)?;
        let total = inputs
            .iter()
            .try_fold(0u64, |n, input| n.checked_add(input.bytes))
            .ok_or(Error::Limit("segment byte sum"))?;
        if inputs.is_empty()
            || inputs.len() > alt_byte_segment::MAX_MEMBERS
            || total > alt_byte_segment::MAX_BYTES
        {
            return Err(Error::Limit("segment inputs"));
        }
        let mut digests: Vec<_> = inputs.iter().map(|input| input.digest).collect();
        digests.sort_unstable();
        if digests.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(Error::Integrity("duplicate segment input"));
        }
        let path = self.segment_path(scope, id)?;
        let parent = path.parent().ok_or(Error::Integrity("segment parent"))?;
        let pending = path.with_extension("pending");
        let mut temp = OpenOptions::new()
            .write(true)
            .read(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(alt_os::sys::O_NOFOLLOW | alt_os::sys::O_CLOEXEC)
            .open(&pending)?;
        // No automatic reuse of a partial pending file. Drop cleans ordinary
        // failures; process death leaves the exact registered ID for recovery.
        let mut cleanup = Pending(Some(pending.clone()));
        boundary(Boundary::Created)?;
        let written = alt_byte_segment::encode(&mut temp, inputs, Limits::default())
            .map_err(segment_error)?;
        boundary(Boundary::Encoded)?;
        temp.sync_all()?;
        boundary(Boundary::TemporarySynced)?;
        let file = match fs::hard_link(&pending, &path) {
            Ok(()) => temp,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                let mut incumbent = Segment::open(open_segment(&path)?, Limits::default())
                    .map_err(segment_error)?;
                if incumbent.index() != &written.index
                    || incumbent.recorded_checksum() != &written.checksum
                {
                    return Err(Error::Precondition(
                        "segment ID already has different bytes",
                    ));
                }
                let before = Stamp::read(incumbent.source())?;
                incumbent
                    .verify_checksum(&written.checksum)
                    .map_err(segment_error)?;
                if before != Stamp::read(incumbent.source())? {
                    return Err(Error::Precondition("incumbent segment changed"));
                }
                incumbent.source().try_clone()?
            }
            Err(e) => return Err(e.into()),
        };
        boundary(Boundary::Installed)?;
        file.sync_all()?;
        boundary(Boundary::InstalledSynced)?;
        fs::remove_file(&pending)?;
        cleanup.0 = None;
        File::open(parent)?.sync_all()?;
        boundary(Boundary::DirectorySynced)?;
        let stamp = Stamp::read(&file)?;
        if stamp != Stamp::read(&open_segment(&path)?)? || stamp.bytes != written.index.file_bytes()
        {
            return Err(Error::Precondition("installed segment changed"));
        }
        let proof = DurableSegment {
            file: Arc::new(file),
            stamp,
            scope: scope.clone(),
            id: id.into(),
            index: Arc::new(written.index),
            checksum: written.checksum,
        };
        // The encoder checked every input and all writes succeeded before the
        // file/directory confirmations. As with loose installation, the
        // mandatory registration step verifies actual stored contents. Keep
        // the encoder's checksum in the private proof for that verification.
        self.check_segment_identity(&proof)?;
        Ok(proof)
    }

    fn check_segment_identity(&self, proof: &DurableSegment) -> Result<()> {
        let path = self.segment_path(&proof.scope, &proof.id)?;
        if Stamp::read(&proof.file)? != proof.stamp
            || Stamp::read(&open_segment(&path)?)? != proof.stamp
        {
            return Err(Error::Precondition("durable segment changed"));
        }
        Ok(())
    }

    /// Revalidate both physical identity and every original byte. Metadata
    /// timestamps alone cannot detect all same-tick in-place corruption.
    pub fn check_segment_proof(&self, proof: &DurableSegment) -> Result<()> {
        self.check_segment_identity(proof)?;
        let path = self.segment_path(&proof.scope, &proof.id)?;
        let mut segment =
            Segment::open(open_segment(&path)?, Limits::default()).map_err(segment_error)?;
        if segment.index() != proof.index.as_ref() {
            return Err(Error::Integrity("durable segment index changed"));
        }
        segment
            .verify_checksum(&proof.checksum)
            .map_err(segment_error)?;
        self.check_segment_identity(proof)
    }

    /// Read a proof member without allocating another descriptor. The proof's
    /// file identity is rechecked before granting the bounded reader.
    pub fn read_durable_segment(
        &self,
        proof: &DurableSegment,
        digest: &[u8; 32],
    ) -> Result<StoredReader> {
        self.check_segment_identity(proof)?;
        let member = proof
            .index
            .find(digest)
            .ok_or(Error::Integrity("missing proof member"))?;
        Ok(StoredReader {
            file: proof.file.clone(),
            start: member.offset(),
            bytes: member.bytes(),
            position: 0,
        })
    }

    /// Check the current file and exact database-selected index/member range.
    /// This does not claim payload integrity: callers must verify canonical
    /// identities through their existing publication/read validation path.
    pub fn read_segment(
        &self,
        scope: &Scope,
        digest: &str,
        bytes: u64,
        location: &SegmentLocation,
    ) -> Result<StoredReader> {
        check_digest(digest)?;
        let digest = alt_hash::blake3::Hash::from_hex(digest)
            .map_err(|_| Error::Integrity("segment digest"))?;
        let path = self.segment_path(scope, &location.segment_id)?;
        let mut file = open_segment(&path)?;
        let index = Segment::open(&mut file, Limits::default())
            .map_err(segment_error)?
            .index()
            .clone();
        let member = index
            .find(digest.as_bytes())
            .ok_or(Error::Integrity("missing segment member"))?;
        if index.digest() != &location.index_digest
            || member.offset() != location.offset
            || member.bytes() != bytes
        {
            return Err(Error::Integrity("segment location differs"));
        }
        Ok(StoredReader {
            file: Arc::new(file),
            start: member.offset(),
            bytes,
            position: 0,
        })
    }

    pub fn verify_segment_member(
        &self,
        scope: &Scope,
        digest: &str,
        bytes: u64,
        location: &SegmentLocation,
    ) -> Result<StoredReader> {
        let mut reader = self.read_segment(scope, digest, bytes, location)?;
        let mut hash = alt_hash::blake3::Hasher::new();
        let mut buffer = [0; 65536];
        loop {
            let n = reader.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            hash.update(&buffer[..n]);
        }
        if hash.finalize().to_hex().as_str() != digest {
            return Err(Error::Integrity("segment member hash"));
        }
        reader.rewind()?;
        Ok(reader)
    }

    /// Coordinator-only operation: first commit a deleting marker and fence
    /// every writer/reader that could still acquire this physical segment.
    pub fn remove_segment(&self, scope: &Scope, id: &str) -> Result<()> {
        let path = self.segment_path(scope, id)?;
        for p in [path.clone(), path.with_extension("pending")] {
            match fs::remove_file(p) {
                Ok(()) => (),
                Err(e) if e.kind() == io::ErrorKind::NotFound => (),
                Err(e) => return Err(e.into()),
            }
        }
        File::open(path.parent().ok_or(Error::Integrity("segment parent"))?)?.sync_all()?;
        Ok(())
    }
}
struct Pending(Option<PathBuf>);
impl Drop for Pending {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            let _ = fs::remove_file(path);
        }
    }
}
fn check_segment_id(id: &str) -> Result<()> {
    if id.len() != 32
        || !id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Error::Integrity("segment ID"));
    }
    Ok(())
}
fn open_segment(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(alt_os::sys::O_NOFOLLOW | alt_os::sys::O_CLOEXEC)
        .open(path)?;
    Stamp::read(&file)?;
    Ok(file)
}
fn segment_error(error: alt_byte_segment::Error) -> Error {
    match error {
        alt_byte_segment::Error::Io(e) => e.into(),
        alt_byte_segment::Error::Format(reason) => Error::Integrity(reason),
        alt_byte_segment::Error::Limit(reason) => Error::Limit(reason),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn scope() -> Scope {
        Scope {
            installation_id: "installation".into(),
            repository_instance_id: "repository".into(),
            recovery_epoch: 1,
        }
    }
    const ID: &str = "abcdefabcdefabcdefabcdefabcdefab";
    const STAGES: [Boundary; 6] = [
        Boundary::Created,
        Boundary::Encoded,
        Boundary::TemporarySynced,
        Boundary::Installed,
        Boundary::InstalledSynced,
        Boundary::DirectorySynced,
    ];

    #[test]
    fn persistence_error_never_returns_proof_and_retry_checks_the_installed_file() {
        for stage in STAGES {
            let temp = alt_os::temp::tempdir().unwrap();
            let store = ByteStore::open(temp.path()).unwrap();
            let digest = *alt_hash::blake3::hash(b"original").as_bytes();
            let mut source = io::Cursor::new(b"original");
            let result = store.put_segment_inner(
                &scope(),
                ID,
                &mut [Input {
                    digest,
                    bytes: 8,
                    source: &mut source,
                }],
                |boundary| {
                    if boundary as u8 == stage as u8 {
                        Err(io::Error::other("injected persistence failure"))
                    } else {
                        Ok(())
                    }
                },
            );
            assert!(result.is_err(), "{stage:?}");
            let path = store.segment_path(&scope(), ID).unwrap();
            assert!(!path.with_extension("pending").exists());
            source.set_position(0);
            let proof = store
                .put_segment(
                    &scope(),
                    ID,
                    &mut [Input {
                        digest,
                        bytes: 8,
                        source: &mut source,
                    }],
                )
                .unwrap();
            store.check_segment_proof(&proof).unwrap();
            let mut actual = Vec::new();
            store
                .read_durable_segment(&proof, &digest)
                .unwrap()
                .read_to_end(&mut actual)
                .unwrap();
            assert_eq!(actual, b"original");
        }
    }

    // Executed in an isolated child by the parent test below. The exit avoids
    // destructors, reproducing real process interruption at every boundary.
    #[test]
    fn interrupted_segment_worker() {
        let Some(root) = std::env::var_os("ALT_SEGMENT_CRASH_ROOT") else {
            return;
        };
        let stage: u8 = std::env::var("ALT_SEGMENT_CRASH_STAGE")
            .unwrap()
            .parse()
            .unwrap();
        let store = ByteStore::open(Path::new(&root)).unwrap();
        let digest = *alt_hash::blake3::hash(b"original").as_bytes();
        let mut source = io::Cursor::new(b"original");
        let _ = store.put_segment_inner(
            &scope(),
            ID,
            &mut [Input {
                digest,
                bytes: 8,
                source: &mut source,
            }],
            |boundary| {
                if boundary as u8 == stage {
                    std::process::exit(73);
                }
                Ok(())
            },
        );
        panic!("crash boundary not reached");
    }

    #[test]
    fn interrupted_files_remain_owned_and_fenced_cleanup_recovers_every_boundary() {
        for stage in STAGES {
            let root = alt_os::temp::tempdir().unwrap();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "bytes::segments::tests::interrupted_segment_worker",
                    "--nocapture",
                ])
                .env("ALT_SEGMENT_CRASH_ROOT", root.path())
                .env("ALT_SEGMENT_CRASH_STAGE", (stage as u8).to_string())
                .output()
                .unwrap();
            assert_eq!(
                output.status.code(),
                Some(73),
                "{stage:?}: {}",
                String::from_utf8_lossy(&output.stdout)
            );
            let store = ByteStore::open(root.path()).unwrap();
            let path = store.segment_path(&scope(), ID).unwrap();
            let pending = path.with_extension("pending");
            assert_eq!(path.exists(), stage as u8 >= Boundary::Installed as u8);
            assert_eq!(
                pending.exists(),
                (stage as u8) < Boundary::DirectorySynced as u8
            );
            if path.exists() {
                Segment::open(open_segment(&path).unwrap(), Limits::default())
                    .unwrap()
                    .verify()
                    .unwrap();
            }
            // Simulates the coordinator after it commits the deleting fence.
            store.remove_segment(&scope(), ID).unwrap();
            assert!(!path.exists() && !pending.exists());
            let digest = *alt_hash::blake3::hash(b"original").as_bytes();
            let mut source = io::Cursor::new(b"original");
            let proof = store
                .put_segment(
                    &scope(),
                    ID,
                    &mut [Input {
                        digest,
                        bytes: 8,
                        source: &mut source,
                    }],
                )
                .unwrap();
            store.check_segment_proof(&proof).unwrap();
        }
    }
}
