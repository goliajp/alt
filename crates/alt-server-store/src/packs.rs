//! Git objects as verified pack segments: one database row per segment, refs and the
//! segment published in one transaction, reads without any per-object row.

use crate::jsonb::JsonText;
use crate::{
    Credential, Error, Result, Store,
    store::{authorize, credential_row, instance, integer, now, snapshot, unsigned},
};
use alt_git_codec::{ObjectId, ObjectKind};
use alt_pack_segment::{Kind, Segment, SegmentSet};
use alt_remote_protocol::{Publication, Receipt, ReceiptBody};
use alt_repository_service::{Graph, validate_publication};
use std::{path::PathBuf, sync::Arc};

/// segments up to this size are stored with their row instead of being flushed to disk
pub const INLINE_SEGMENT_BYTES: usize = 65536;

pub struct NewSegment<'a> {
    pub id: &'a str,
    pub objects: u64,
    pub bytes: u64,
    pub flags: u32,
    /// the whole segment file when it is small enough to be stored with its row
    pub inline: Option<&'a [u8]>,
}

/// where a repository's segment lives inside a byte volume
pub(crate) fn segment_file(
    root: &std::path::Path,
    repository: &str,
    segment: &str,
) -> Result<PathBuf> {
    if segment.len() != 32 || !segment.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(Error::Integrity("segment id"));
    }
    Ok(root
        .join("packs")
        .join(&alt_hash::blake3::hash(repository.as_bytes()).to_hex()[..32])
        .join(format!("{segment}.seg")))
}

impl crate::ByteStore {
    /// Writes one segment file of a verified backup into a fresh volume. The row that
    /// makes it live comes from the restored database.
    pub fn restore_pack_segment(
        &self,
        repository: &str,
        segment: &str,
        bytes: u64,
        digest: &str,
        source: &mut impl std::io::Read,
    ) -> Result<()> {
        use std::{io::Write, os::unix::fs::OpenOptionsExt};
        let path = segment_file(self.root(), repository, segment)?;
        let directory = path.parent().expect("segment directory");
        std::fs::create_dir_all(directory)?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        let mut hasher = alt_hash::blake3::Hasher::new();
        let mut buffer = vec![0u8; 1 << 16];
        let mut written = 0u64;
        loop {
            let count = source.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            hasher.update(&buffer[..count]);
            file.write_all(&buffer[..count])?;
            written += count as u64;
        }
        if written != bytes || hasher.finalize().to_hex().as_str() != digest {
            drop(file);
            let _ = std::fs::remove_file(&path);
            return Err(Error::Integrity("restored pack segment bytes"));
        }
        file.sync_all()?;
        std::fs::File::open(directory)?.sync_all()?;
        std::fs::File::open(self.root().join("packs"))?.sync_all()?;
        Ok(())
    }
}

pub(crate) fn object_kind(kind: Kind) -> ObjectKind {
    match kind {
        Kind::Commit => ObjectKind::Commit,
        Kind::Tree => ObjectKind::Tree,
        Kind::Blob => ObjectKind::Blob,
        Kind::Tag => ObjectKind::Tag,
    }
}

/// The objects of a repository's segments as a graph: commits from the stored commit
/// graph, everything else read on demand.
pub struct SegmentGraph<'a> {
    pub set: &'a SegmentSet,
    pub limits: alt_repository_service::GraphLimits,
}

impl Graph for SegmentGraph<'_> {
    fn node(
        &self,
        oid: ObjectId,
    ) -> alt_repository_service::Result<std::borrow::Cow<'_, alt_repository_service::Node>> {
        use alt_repository_service::{Error as E, Node, parse_node};
        let key: [u8; 20] = oid.as_bytes().try_into().map_err(|_| E::Missing(oid))?;
        let id = |oid: &[u8; 20]| {
            ObjectId::from_bytes(alt_git_codec::HashAlgo::Sha1, oid).expect("sha1 object id")
        };
        if let Some(commit) = self.set.commit(&key) {
            return Ok(std::borrow::Cow::Owned(Node::Commit {
                tree: id(&commit.tree),
                parents: commit.parents.iter().map(id).collect(),
            }));
        }
        let kind = match self.set.kind(&key) {
            Ok(Some(Kind::Blob)) => return Ok(std::borrow::Cow::Owned(Node::Blob)),
            Ok(Some(Kind::Tree)) => ObjectKind::Tree,
            Ok(Some(Kind::Tag)) => ObjectKind::Tag,
            Ok(Some(Kind::Commit)) => {
                return Err(E::Integrity("commit missing from the commit graph"));
            }
            Ok(None) => return Err(E::Missing(oid)),
            Err(_) => return Err(E::Integrity("pack segment record")),
        };
        let (_, data) = self
            .set
            .read(&key)
            .map_err(|_| E::Integrity("pack segment object"))?;
        parse_node(kind, &data, alt_git_codec::HashAlgo::Sha1).map(std::borrow::Cow::Owned)
    }

    fn graph_limits(&self) -> alt_repository_service::GraphLimits {
        self.limits
    }
}

/// directory holding one repository's segments, created on first use
pub(crate) fn pack_directory_at(root: &std::path::Path, repository: &str) -> Result<PathBuf> {
    let packs = root.join("packs");
    let path = packs.join(&alt_hash::blake3::hash(repository.as_bytes()).to_hex()[..32]);
    if !path.is_dir() {
        std::fs::create_dir_all(&path)?;
        std::fs::File::open(&packs)?.sync_all()?;
        std::fs::File::open(root)?.sync_all()?;
    }
    Ok(path)
}

/// a segment built from natively uploaded Git objects, not yet published
pub(crate) struct Materialized {
    pub(crate) id: String,
    pub(crate) objects: u64,
    pub(crate) bytes: u64,
    pub(crate) flags: u32,
    pub(crate) inline: Option<Vec<u8>>,
    pub(crate) segment: Arc<Segment>,
    pub(crate) path: PathBuf,
}

pub(crate) fn insert_segment(
    tx: &mut crate::database::Transaction<'_>,
    repository: &str,
    sequence: u64,
    segment: &NewSegment<'_>,
) -> Result<()> {
    tx.execute(
        "INSERT INTO alt_pack_segments(repository,segment_id,sequence,objects,bytes,flags,state,inline) VALUES($1,$2,$3,$4,$5,$6,'live',$7)",
        &[&repository, &segment.id, &integer(sequence)?, &integer(segment.objects)?, &integer(segment.bytes)?, &(segment.flags as i32), &segment.inline],
    )?;
    Ok(())
}

impl Store {
    /// A Git object of the repository, read from its segments. Every published Git
    /// object lives there, whichever protocol brought it in.
    pub fn open_published_object(
        &mut self,
        credential: &Credential,
        repository: &str,
        oid: ObjectId,
    ) -> Result<(ObjectKind, u64, alt_pack_segment::ObjectStream)> {
        let view = self.pack_view(credential, repository)?;
        let key: [u8; 20] = oid.as_bytes().try_into().map_err(|_| Error::Unauthorized)?;
        let (kind, size, stream) = view
            .segments
            .open(&key)
            .map_err(|_| Error::Integrity("published object bytes"))?
            .ok_or(Error::Unauthorized)?;
        if size > self.limits.object_bytes {
            return Err(Error::Limit("published object read bytes"));
        }
        Ok((object_kind(kind), size, stream))
    }

    /// live segments of a repository, read inside the caller's transaction
    pub(crate) fn live_segment_ids(
        tx: &mut crate::database::Transaction<'_>,
        repository: &str,
    ) -> Result<Vec<(String, bool)>> {
        Ok(tx
            .query("SELECT segment_id,inline IS NOT NULL FROM alt_pack_segments WHERE repository=$1 AND state='live' ORDER BY sequence", &[&repository])?
            .iter()
            .map(|row| (row.get(0), row.get(1)))
            .collect())
    }

    pub fn pack_directory(&self, repository: &str) -> Result<PathBuf> {
        pack_directory_at(self.bytes().root(), repository)
    }

    pub fn pack_path(&self, repository: &str, segment: &str) -> Result<PathBuf> {
        self.pack_directory(repository)?;
        segment_file(self.bytes().root(), repository, segment)
    }

    /// makes a segment this process just wrote available to later requests
    pub fn adopt_segment(&self, repository: &str, id: &str, segment: Arc<Segment>) -> Result<()> {
        crate::open_segments::insert(self.pack_path(repository, id)?, segment);
        Ok(())
    }

    /// `ids` pairs each live segment with whether its bytes are still held by its row
    pub(crate) fn open_segments(
        &mut self,
        repository: &str,
        ids: &[(String, bool)],
    ) -> Result<SegmentSet> {
        let mut segments = Vec::with_capacity(ids.len());
        for (id, inline) in ids {
            let path = self.pack_path(repository, id)?;
            let cached = crate::open_segments::get(&path);
            let segment = match cached {
                Some(segment) => segment,
                None => {
                    if *inline {
                        // the file of an unsettled segment was never flushed; the row is the truth
                        let bytes: Vec<u8> = self
                            .client
                            .query_one("SELECT inline FROM alt_pack_segments WHERE repository=$1 AND segment_id=$2", &[&repository, id])?
                            .get::<_, Option<Vec<u8>>>(0)
                            .ok_or(Error::Integrity("pack segment bytes"))?;
                        let staging = path.with_extension("restore");
                        std::fs::write(&staging, &bytes)?;
                        std::fs::rename(&staging, &path)?;
                    }
                    let segment = Arc::new(
                        Segment::open(&path)
                            .map_err(|_| Error::Integrity("pack segment unavailable"))?,
                    );
                    crate::open_segments::insert(path, segment.clone());
                    segment
                }
            };
            segments.push(segment);
        }
        Ok(SegmentSet::new(segments))
    }

    /// Publishes ref updates, and the segment that carries their new objects, in one
    /// transaction. The segment file must already be durable under its final name.
    pub fn pack_publish(
        &mut self,
        credential: &Credential,
        publication: &Publication,
        graph: &impl Graph,
        segment: Option<NewSegment<'_>>,
    ) -> Result<Receipt> {
        let repository = &publication.key.scope.repository_instance_id;
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let current = snapshot(&mut tx, &self.spec, repository)?;
        let auth = authorize(&mut tx, &self.spec, credential, &current)?;
        let time = now(&mut tx)?;
        let validated =
            validate_publication(publication, &current, &auth, graph, self.limits, time)?;
        crate::publication::replace_refs(&mut tx, repository, &validated.updates)?;
        let unreachable = publication
            .refs
            .iter()
            .any(|update| update.force || update.new.is_none());
        let sequence = unsigned(
            tx.query_one(
                "UPDATE alt_repositories SET transaction_sequence=transaction_sequence+1,pack_unreachable=pack_unreachable OR $2 WHERE id=$1 RETURNING transaction_sequence",
                &[repository, &unreachable],
            )?
            .get(0),
        )?;
        if let Some(segment) = &segment {
            if tx.execute(
                "UPDATE alt_repositories SET stored_bytes=stored_bytes+$2 WHERE id=$1 AND $2<=quota_bytes-stored_bytes-reserved_bytes-link_bytes",
                &[repository, &integer(segment.bytes)?],
            )? != 1
            {
                return Err(Error::Limit("repository quota"));
            }
            insert_segment(&mut tx, repository, sequence, segment)?;
        }
        let body = ReceiptBody {
            publication: publication.clone(),
            request_digest: validated.digest.clone(),
            transaction_sequence: sequence,
            policy_version: validated.policy_version,
            committed_at_unix_ms: now(&mut tx)?,
        };
        let signature = self.signing_key.sign(&body.signing_bytes_of_validated()?);
        let receipt = Receipt {
            body,
            signing_key_id: alt_hash::blake3::hash(self.signing_key.verifying_key().as_bytes())
                .to_hex()
                .to_string(),
            signature: signature
                .to_bytes()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        };
        let documents = crate::committed::committed_documents(&receipt)?;
        crate::committed::record_committed(
            &mut tx,
            repository,
            sequence,
            &auth.actor_id,
            &documents,
        )?;
        tx.execute(
            "INSERT INTO alt_requests(repository,epoch,actor_id,request_id,digest,publication,manifest,state,result,expires_at,reserved_bytes) VALUES($1,$2,$3,$4,$5,$6,$7,'committed',$8,0,0)",
            &[
                repository,
                &integer(publication.key.scope.recovery_epoch)?,
                &publication.key.actor_id,
                &publication.key.request_id,
                &validated.digest,
                &JsonText(&alt_json::to_vec(publication)?),
                &JsonText(&alt_json::to_vec(&alt_json::json!({"pack_segment": segment.as_ref().map(|segment| segment.id)}))?),
                &JsonText(&documents.result),
            ],
        )?;
        // lock waits can outlive a lease checked on entry
        credential_row(&mut tx, &self.spec, credential)?;
        crate::cloud_quota::check_publication(&mut tx, &self.spec)?;
        tx.commit().map_err(|_| Error::OutcomeUnknown)?;
        Ok(receipt)
    }
}
