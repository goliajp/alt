use crate::{Blob, BlobSource, Envelope, Error, PackageLimits, Result, read_metadata};
use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_remote_protocol::{ContentKind, Manifest, Publication, Scope};
use alt_repository_service::{ObjectReader, ObjectSource, VerifiedGraph};
use alt_task_service::{ArtifactSource, TaskLimits, VerifiedPublication};
use std::{cell::Cell, collections::BTreeMap, io::Read};

/// Aggregate work is bounded across the entire import, including shared
/// objects reread by different historical publications. Per-graph limits alone
/// would allow a small archive to amplify work through many overlapping roots.
#[derive(Clone, Copy, Debug)]
pub struct VerificationLimits {
    pub package: PackageLimits,
    pub task: TaskLimits,
    pub work_bytes: u64,
    pub reads: usize,
}
struct Source<'a, S> {
    inner: &'a S,
    blobs: BTreeMap<&'a str, &'a Blob>,
    objects: BTreeMap<ObjectId, &'a crate::Object>,
    bytes: Cell<u64>,
    reads: Cell<usize>,
}
impl<S: BlobSource> BlobSource for Source<'_, S> {
    fn open(&self, blob: &Blob) -> Result<Box<dyn Read + '_>> {
        if self.blobs.get(blob.blake3.as_str()).copied() != Some(blob) {
            return Err(Error::Invalid("read outside the catalog"));
        }
        self.bytes.set(
            self.bytes
                .get()
                .checked_sub(blob.bytes)
                .ok_or(Error::Limit("verification bytes"))?,
        );
        self.reads.set(
            self.reads
                .get()
                .checked_sub(1)
                .ok_or(Error::Limit("verification reads"))?,
        );
        self.inner.open(blob)
    }
}
impl<S: BlobSource> ObjectSource for Source<'_, S> {
    fn open(&self, oid: ObjectId) -> alt_repository_service::Result<Option<ObjectReader<'_>>> {
        let Some(object) = self.objects.get(&oid) else {
            return Ok(None);
        };
        let kind = match object.kind {
            ContentKind::Blob => ObjectKind::Blob,
            ContentKind::Tree => ObjectKind::Tree,
            ContentKind::Commit => ObjectKind::Commit,
            ContentKind::Tag => ObjectKind::Tag,
            _ => {
                return Err(alt_repository_service::Error::Integrity(
                    "portable object kind",
                ));
            }
        };
        Ok(Some(ObjectReader {
            kind,
            bytes: object.payload.bytes,
            reader: BlobSource::open(self, &object.payload).map_err(|e| {
                alt_repository_service::Error::Io(std::io::Error::other(e.to_string()))
            })?,
        }))
    }
}
impl<S: BlobSource> ArtifactSource for Source<'_, S> {
    fn open(&self, artifact: &alt_task::Artifact) -> alt_task_service::Result<Box<dyn Read + '_>> {
        BlobSource::open(
            self,
            &Blob {
                blake3: artifact.blake3.clone(),
                bytes: artifact.bytes,
            },
        )
        .map_err(|e| alt_task_service::Error::Io(std::io::Error::other(e.to_string())))
    }
}

/// Checked immutable bytes and history, with no current approval or authority.
/// Only the verifier constructs this value. Adapters must keep the same bytes
/// pinned through activation and recheck their own transaction preconditions.
pub struct VerifiedSnapshot {
    catalog_digest: String,
    graph: VerifiedGraph,
    publications: Vec<(String, VerifiedPublication)>,
}
impl VerifiedSnapshot {
    pub fn catalog_digest(&self) -> &str {
        &self.catalog_digest
    }
    pub fn graph(&self) -> &VerifiedGraph {
        &self.graph
    }
    pub fn publications(&self) -> &[(String, VerifiedPublication)] {
        &self.publications
    }
    pub fn verify(
        envelope: &Envelope,
        expected: &Scope,
        pinned_key: &[u8; 32],
        source: &impl BlobSource,
        limits: VerificationLimits,
    ) -> Result<Self> {
        if limits.work_bytes == 0 || limits.reads == 0 {
            return Err(Error::Limit("verification budget"));
        }
        let catalog_digest = envelope.verify(expected, pinned_key, limits.package)?;
        let catalog = &envelope.catalog;
        let objects = catalog
            .objects
            .iter()
            .map(|object| {
                Ok((
                    object
                        .oid
                        .parse()
                        .map_err(|_| Error::Invalid("object id"))?,
                    object,
                ))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        let bounded = Source {
            inner: source,
            blobs: catalog
                .blobs
                .iter()
                .map(|b| (b.blake3.as_str(), b))
                .collect(),
            objects,
            bytes: Cell::new(limits.work_bytes),
            reads: Cell::new(limits.reads),
        };
        // Recheck staged bytes. An intact incoming stream alone cannot prove
        // that a destination sink successfully retained those bytes afterward.
        for blob in &catalog.blobs {
            let mut reader = BlobSource::open(&bounded, blob)?;
            let mut hash = alt_hash::blake3::Hasher::new();
            let mut remaining = blob.bytes;
            let mut buffer = [0u8; 65536];
            while remaining > 0 {
                let count = reader.read(&mut buffer[..remaining.min(65536) as usize])?;
                if count == 0 {
                    return Err(Error::Invalid("staged blob truncated"));
                }
                hash.update(&buffer[..count]);
                remaining -= count as u64;
            }
            if reader.read(&mut [0])? != 0 || hash.finalize().to_hex().as_str() != blob.blake3 {
                return Err(Error::Invalid("staged blob identity"));
            }
        }
        crate::verify_publications(catalog, &bounded, limits.package)?;
        crate::import::verify_history(catalog, &bounded, limits.package)?;
        let roots = bounded.objects.keys().copied().collect::<Vec<_>>();
        let algo = match catalog.object_format {
            alt_remote_protocol::ObjectFormat::Sha1 => HashAlgo::Sha1,
            alt_remote_protocol::ObjectFormat::Sha256 => HashAlgo::Sha256,
        };
        // All declared objects, including unreachable retained history, must
        // have correct Git IDs, edge types, and a complete dependency closure.
        let graph = alt_repository_service::verify_graph(
            &bounded,
            algo,
            &roots,
            limits.package.protocol.into(),
        )?;
        let mut publications = Vec::new();
        for item in &catalog.publications {
            let publication = Publication::decode(
                &read_metadata(&bounded, &item.publication, limits.package.metadata_bytes)?,
                limits.package.protocol,
            )?;
            let manifest = Manifest::decode(
                &read_metadata(&bounded, &item.manifest, limits.package.metadata_bytes)?,
                limits.package.protocol,
            )?;
            let verified = VerifiedPublication::verify(
                &publication,
                &manifest,
                &bounded,
                &bounded,
                limits.package.protocol,
                limits.task,
            )?;
            publications.push((item.digest.clone(), verified));
        }
        Ok(Self {
            catalog_digest,
            graph,
            publications,
        })
    }
}
